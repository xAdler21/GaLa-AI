//! Angebote: Commands für LV-Liste, Import, Preise und Export. Exporte landen im
//! Download-Ordner des Nutzers.

use std::path::{Path, PathBuf};

use api_types::client::{
    LvDetail, LvExport, LvImportRequest, LvSummary, PriceEntry, PriceUpdateRequest,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use reqwest::Method;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::AppState;
use crate::net::{self, CmdResult, err};

/// IDs kommen aus der eigenen Oberfläche; trotzdem nur UUID-Zeichen in den Pfad lassen.
fn check_id(id: &str) -> CmdResult<&str> {
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(id)
    } else {
        Err(err("invalid_request", "Ungültige LV-ID."))
    }
}

#[tauri::command]
pub async fn lv_list(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Vec<LvSummary>> {
    net::authed(&app, &state.http, Method::GET, "/lvs", None::<&()>).await
}

#[tauri::command]
pub async fn lv_import(
    app: AppHandle,
    state: State<'_, AppState>,
    file_name: String,
    data: Vec<u8>,
) -> CmdResult<LvDetail> {
    let body = LvImportRequest {
        file_name,
        data_base64: B64.encode(data),
    };
    net::authed(&app, &state.http, Method::POST, "/lvs", Some(&body)).await
}

#[tauri::command]
pub async fn lv_get(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<LvDetail> {
    let path = format!("/lvs/{}", check_id(&id)?);
    net::authed(&app, &state.http, Method::GET, &path, None::<&()>).await
}

#[tauri::command]
pub async fn lv_set_prices(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    revision: i32,
    prices: Vec<PriceEntry>,
) -> CmdResult<LvDetail> {
    let path = format!("/lvs/{}/prices", check_id(&id)?);
    let body = PriceUpdateRequest { revision, prices };
    net::authed(&app, &state.http, Method::PUT, &path, Some(&body)).await
}

#[tauri::command]
pub async fn lv_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let path = format!("/lvs/{}", check_id(&id)?);
    net::authed(&app, &state.http, Method::DELETE, &path, None::<&()>).await
}

#[derive(Serialize)]
pub struct SavedExport {
    path: String,
    missing_prices: Vec<String>,
}

/// Zeichen, die Windows in Dateinamen nicht erlaubt, werden zu `_`.
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "LV".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Freier Name im Ordner: `Kita.D84`, sonst `Kita (2).D84` usw.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let with = |n: usize| {
        let base = if n == 1 {
            stem.to_owned()
        } else {
            format!("{stem} ({n})")
        };
        dir.join(if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        })
    };
    (1..)
        .map(with)
        .find(|p| !p.exists())
        .expect("unendliche Folge")
}

#[tauri::command]
pub async fn lv_export(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    format: String,
) -> CmdResult<SavedExport> {
    if !matches!(format.as_str(), "x84" | "d84" | "xlsx") {
        return Err(err("invalid_request", "Unbekanntes Exportformat."));
    }
    let path = format!("/lvs/{}/export/{format}", check_id(&id)?);
    let out: LvExport = net::authed(&app, &state.http, Method::GET, &path, None::<&()>).await?;
    let data = B64
        .decode(out.data_base64)
        .map_err(|_| err("bad_response", "Exportdatei vom Server ist beschädigt."))?;
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| err("io", format!("Download-Ordner nicht gefunden: {e}")))?;
    let target = free_path(&dir, &safe_name(&out.file_name));
    std::fs::write(&target, data)
        .map_err(|e| err("io", format!("{} nicht schreibbar: {e}", target.display())))?;
    Ok(SavedExport {
        path: target.display().to_string(),
        missing_prices: out.missing_prices,
    })
}

/// Öffnet den Explorer mit markierter Datei.
#[tauri::command]
pub fn show_in_folder(path: String) -> CmdResult<()> {
    let p = Path::new(&path);
    if !p.is_file() {
        return Err(err("io", "Datei nicht gefunden."));
    }
    std::process::Command::new("explorer")
        .arg(format!("/select,{}", p.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| err("io", format!("Explorer nicht startbar: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_free() {
        assert_eq!(safe_name("LV: Kita/Süd?.D84"), "LV_ Kita_Süd_.D84");
        assert_eq!(safe_name(" .. "), "LV");
        assert!(check_id("0f2c-AB").is_ok() && check_id("../x").is_err());
        let dir = std::env::temp_dir().join(format!("gala-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.D84"), b"x").unwrap();
        assert_eq!(free_path(&dir, "a.D84"), dir.join("a (2).D84"));
        assert_eq!(free_path(&dir, "b.xlsx"), dir.join("b.xlsx"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
