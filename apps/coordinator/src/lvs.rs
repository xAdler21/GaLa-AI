//! Angebote: LV importieren, Einheitspreise setzen, Abgabe (X84/D84) und Excel exportieren.

use api_types::client::{
    ExportFormat, LvDetail, LvExport, LvImportRequest, LvSummary, PriceUpdateRequest,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use db::lvs::{LvFull, LvRow, LvStats, NewLv};
use lv_model::{Decimal, Field, Lv, LvSource};
use uuid::Uuid;

use crate::api::AuthUser;
use crate::error::ApiErr;
use crate::state::AppState;

/// Größte LV-Datei; GAEB-Dateien sind selten größer als ein paar MB.
const MAX_FILE: usize = 30 * 1024 * 1024;

/// Bietername für Satz 04 der D84 (Einstellung `bidder_name`, String).
const KEY_BIDDER: &str = "bidder_name";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/lvs",
            get(list)
                .post(import)
                // Base64 bläht um ein Drittel auf.
                .layer(DefaultBodyLimit::max(MAX_FILE / 3 * 4 + 64 * 1024)),
        )
        .route("/api/v1/lvs/{id}", get(detail).delete(remove))
        .route("/api/v1/lvs/{id}/prices", put(set_prices))
        .route("/api/v1/lvs/{id}/export/{format}", get(export))
}

fn stats(lv: &Lv) -> LvStats {
    let (mut positions, mut priced) = (0, 0);
    for p in lv.positions() {
        positions += 1;
        if p.ep.value.is_some() {
            priced += 1;
        }
    }
    LvStats {
        positions,
        priced,
        total: lv.total().to_string(),
    }
}

fn summary(r: LvRow) -> LvSummary {
    LvSummary {
        id: r.id,
        name: r.name,
        format: r.format,
        file_name: r.file_name,
        positions: r.positions.max(0) as u32,
        priced: r.priced.max(0) as u32,
        total: r.total.parse().unwrap_or_default(),
        revision: r.revision,
        created_by: r.created_by,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

/// Welche Abgabe aus dem Original möglich ist: X84 aus GAEB XML, D84 aus GAEB 90.
fn exports(format: &str) -> Vec<ExportFormat> {
    let mut out = Vec::new();
    match format.chars().next() {
        Some('X') => out.push(ExportFormat::X84),
        Some('D') => out.push(ExportFormat::D84),
        _ => {}
    }
    out.push(ExportFormat::Xlsx);
    out
}

fn parse_lv(full: &LvFull) -> Result<Lv, ApiErr> {
    serde_json::from_value(full.lv.clone()).map_err(|e| {
        tracing::error!(id = %full.row.id, error = %e, "LV in der Datenbank nicht lesbar");
        ApiErr::internal()
    })
}

fn detail_of(full: LvFull, lv: Lv) -> LvDetail {
    let warnings = serde_json::from_value(full.warnings).unwrap_or_default();
    let exports = exports(&full.row.format);
    LvDetail {
        summary: summary(full.row),
        lv,
        warnings,
        exports,
    }
}

async fn load(state: &AppState, id: Uuid) -> Result<LvFull, ApiErr> {
    db::lvs::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiErr::not_found("LV nicht gefunden"))
}

async fn list(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<LvSummary>>, ApiErr> {
    let rows = db::lvs::list(&state.db).await?;
    Ok(Json(rows.into_iter().map(summary).collect()))
}

async fn import(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    payload: Result<Json<LvImportRequest>, JsonRejection>,
) -> Result<Json<LvDetail>, ApiErr> {
    let Json(req) = payload.map_err(|e| ApiErr::bad_request(format!("Ungültige Anfrage: {e}")))?;
    let bytes = B64
        .decode(req.data_base64.trim())
        .map_err(|_| ApiErr::bad_request("Dateiinhalt ist kein gültiges Base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_FILE {
        return Err(ApiErr::bad_request(format!(
            "Datei leer oder größer als {} MB",
            MAX_FILE / 1024 / 1024
        )));
    }
    let file_name = req.file_name.trim().to_string();
    let imp = gaeb::import(&bytes).map_err(|e| {
        ApiErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_file",
            format!("Keine lesbare GAEB-Datei: {e}"),
        )
    })?;
    let mut lv = imp.lv;
    if lv.name.trim().is_empty() {
        lv.name = file_stem(&file_name).to_string();
    }
    let format = match &lv.source {
        LvSource::Gaeb { format } => format.clone(),
        _ => imp.phase.code(),
    };
    let lv_json = serde_json::to_value(&lv).map_err(|_| ApiErr::internal())?;
    let id = db::lvs::insert(
        &state.db,
        NewLv {
            name: &lv.name,
            format: &format,
            file_name: &file_name,
            original: &bytes,
            lv: lv_json,
            warnings: serde_json::json!(imp.warnings),
            stats: stats(&lv),
            created_by: user.user_id,
        },
    )
    .await?;
    db::audit::record(
        &state.db,
        Some(user.user_id),
        "lv.import",
        serde_json::json!({ "id": id, "file": file_name, "format": format }),
    )
    .await;
    let full = load(&state, id).await?;
    Ok(Json(detail_of(full, lv)))
}

async fn detail(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<LvDetail>, ApiErr> {
    let full = load(&state, id).await?;
    let lv = parse_lv(&full)?;
    Ok(Json(detail_of(full, lv)))
}

/// Setzt EP und rechnet GP = Menge × EP neu; ohne Menge ist der GP der EP (wie im X84/D84).
fn apply_prices(lv: &mut Lv, req: &PriceUpdateRequest) -> Result<(), ApiErr> {
    for entry in &req.prices {
        if entry.ep.is_some_and(|ep| ep < Decimal::ZERO) {
            return Err(ApiErr::bad_request(format!(
                "Negativer Preis bei {}",
                entry.oz
            )));
        }
        let p = lv
            .position_mut(&entry.oz)
            .ok_or_else(|| ApiErr::bad_request(format!("Position {} gibt es nicht", entry.oz)))?;
        match entry.ep {
            Some(ep) => {
                p.ep = Field::certain(ep);
                p.gp = Field::certain(p.computed_gp().unwrap_or(ep));
            }
            None => {
                p.ep = Field::absent();
                p.gp = Field::absent();
            }
        }
    }
    Ok(())
}

async fn set_prices(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
    payload: Result<Json<PriceUpdateRequest>, JsonRejection>,
) -> Result<Json<LvDetail>, ApiErr> {
    let Json(req) = payload.map_err(|e| ApiErr::bad_request(format!("Ungültige Anfrage: {e}")))?;
    let full = load(&state, id).await?;
    if full.row.revision != req.revision {
        return Err(conflict());
    }
    let mut lv = parse_lv(&full)?;
    apply_prices(&mut lv, &req)?;
    let lv_json = serde_json::to_value(&lv).map_err(|_| ApiErr::internal())?;
    db::lvs::update(&state.db, id, req.revision, lv_json, stats(&lv))
        .await?
        .ok_or_else(conflict)?;
    let full = load(&state, id).await?;
    Ok(Json(detail_of(full, lv)))
}

fn conflict() -> ApiErr {
    ApiErr::new(
        StatusCode::CONFLICT,
        "conflict",
        "Das LV wurde inzwischen von jemand anderem gespeichert. Bitte neu laden.",
    )
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiErr> {
    if !db::lvs::delete(&state.db, id).await? {
        return Err(ApiErr::not_found("LV nicht gefunden"));
    }
    db::audit::record(
        &state.db,
        Some(user.user_id),
        "lv.delete",
        serde_json::json!({ "id": id }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn file_stem(name: &str) -> &str {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.').map_or(base, |(stem, _)| stem)
}

async fn export(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, format)): Path<(Uuid, String)>,
) -> Result<Json<LvExport>, ApiErr> {
    let format = ExportFormat::parse(&format)
        .ok_or_else(|| ApiErr::bad_request("Format muss x84, d84 oder xlsx sein"))?;
    let full = load(&state, id).await?;
    if !exports(&full.row.format).contains(&format) {
        return Err(ApiErr::bad_request(format!(
            "{} ist aus einer {}-Datei nicht möglich",
            format.as_str().to_uppercase(),
            full.row.format
        )));
    }
    let lv = parse_lv(&full)?;
    let stem = file_stem(&full.row.file_name);
    let write_err = |e: &dyn std::fmt::Display| {
        tracing::error!(%id, error = %e, "Export fehlgeschlagen");
        ApiErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "export_failed",
            format!("Export fehlgeschlagen: {e}"),
        )
    };
    let (file_name, data, missing) = match format {
        ExportFormat::X84 => {
            let xml = gaeb::decode(&full.original).map_err(|e| write_err(&e))?;
            let out = gaeb::write_x84(&xml, &lv).map_err(|e| write_err(&e))?;
            (
                format!("{stem}.X84"),
                out.xml.into_bytes(),
                out.missing_prices,
            )
        }
        ExportFormat::D84 => {
            let bidder = db::settings::get_raw(&state.db, KEY_BIDDER)
                .await?
                .and_then(|v| v.as_str().map(str::to_string))
                .filter(|s| !s.trim().is_empty());
            let out = gaeb::write_d84(&full.original, &lv, bidder.as_deref())
                .map_err(|e| write_err(&e))?;
            (format!("{stem}.D84"), out.bytes, out.missing_prices)
        }
        ExportFormat::Xlsx => {
            let opts = export::XlsxOptions {
                langtext: true,
                ..Default::default()
            };
            let bytes = export::lv_to_xlsx(&lv, &opts).map_err(|e| write_err(&e))?;
            let missing = lv
                .positions()
                .filter(|p| p.ep.value.is_none())
                .filter_map(|p| p.oz.value.clone())
                .collect();
            (format!("{stem}.xlsx"), bytes, missing)
        }
    };
    db::audit::record(
        &state.db,
        Some(user.user_id),
        "lv.export",
        serde_json::json!({ "id": id, "format": format.as_str(), "missing_prices": missing.len() }),
    )
    .await;
    Ok(Json(LvExport {
        file_name,
        data_base64: B64.encode(data),
        missing_prices: missing.iter().map(ToString::to_string).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use api_types::client::PriceEntry;
    use lv_model::{Node, Oz, Position};

    fn lv() -> Lv {
        let mut p = Position::new(Oz::parse("01.0010").unwrap(), "Rasen");
        p.menge = Field::certain(Decimal::new(125, 1));
        let pauschal = Position::new(Oz::parse("01.0020").unwrap(), "Baustelle");
        Lv {
            name: "Test".into(),
            source: LvSource::Gaeb {
                format: "D83".into(),
            },
            currency: "EUR".into(),
            oz_mask: None,
            nodes: vec![
                Node::Position(Box::new(p)),
                Node::Position(Box::new(pauschal)),
            ],
        }
    }

    fn entry(oz: &str, ep: Option<Decimal>) -> PriceEntry {
        PriceEntry {
            oz: Oz::parse(oz).unwrap(),
            ep,
        }
    }

    #[test]
    fn prices_set_gp_and_stats() {
        let mut lv = lv();
        let req = PriceUpdateRequest {
            revision: 1,
            prices: vec![
                entry("01.0010", Some(Decimal::new(333, 2))),
                entry("01.0020", Some(Decimal::new(500, 0))),
            ],
        };
        apply_prices(&mut lv, &req).unwrap();
        let ps: Vec<_> = lv.positions().collect();
        // 12,5 × 3,33 = 41,625 → 41,63; Pauschale ohne Menge: GP = EP.
        assert_eq!(ps[0].gp.value, Some(Decimal::new(4163, 2)));
        assert_eq!(ps[1].gp.value, Some(Decimal::new(500, 0)));
        let s = stats(&lv);
        assert_eq!((s.positions, s.priced, s.total.as_str()), (2, 2, "541.63"));

        apply_prices(
            &mut lv,
            &PriceUpdateRequest {
                revision: 2,
                prices: vec![entry("01.0010", None)],
            },
        )
        .unwrap();
        assert_eq!(stats(&lv).priced, 1);
    }

    #[test]
    fn rejects_unknown_oz_and_negative_price() {
        let mut lv = lv();
        let unknown = PriceUpdateRequest {
            revision: 1,
            prices: vec![entry("09.0001", Some(Decimal::ONE))],
        };
        assert_eq!(
            apply_prices(&mut lv, &unknown).unwrap_err().code,
            "bad_request"
        );
        let negative = PriceUpdateRequest {
            revision: 1,
            prices: vec![entry("01.0010", Some(Decimal::new(-1, 0)))],
        };
        assert!(apply_prices(&mut lv, &negative).is_err());
    }

    #[test]
    fn exports_by_format_and_stem() {
        assert_eq!(exports("X83"), [ExportFormat::X84, ExportFormat::Xlsx]);
        assert_eq!(exports("D83"), [ExportFormat::D84, ExportFormat::Xlsx]);
        assert_eq!(file_stem("C:\\LV\\Kita Süd.d83"), "Kita Süd");
        assert_eq!(file_stem("ohne_endung"), "ohne_endung");
    }
}
