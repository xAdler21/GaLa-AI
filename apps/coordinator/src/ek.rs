//! Einkaufspreise: Lieferantenangebote und Auftragsbestätigungen im Archiv werden im Hintergrund
//! mit dem LLM ausgelesen (ein Beleg nach dem anderen, neueste zuerst). So bleibt der zweite
//! Platz des llama-server für den Chatbot frei.

use std::time::Duration;

use chrono::NaiveDate;
use db::ek::{Candidate, EkRow};
use llm::{ChatMessage, ChatOptions, LlmClient, LlmError};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Längere Belege werden gekürzt; die Positionen stehen fast immer vorn.
const MAX_CHARS: i32 = 12_000;
/// Ein Beleg mit vielen Positionen braucht bei ~20 Token/s einige Minuten.
const LLM_TIMEOUT: Duration = Duration::from_secs(600);

const SYSTEM: &str = "Du liest Angebote und Auftragsbestätigungen von Lieferanten (Baustoffe, Pflanzen, \
Geräte) an einen Garten- und Landschaftsbaubetrieb. Gib jede Artikelposition mit Preis zurück. \
bezeichnung: Artikelname mit den wichtigsten Maßen, Sorte oder Qualität, ohne Artikelnummer. \
einzelpreis_netto: Preis für 1 Einheit nach Rabatt, ohne MwSt (bei Preiseinheit 100 durch 100 teilen). \
gesamtpreis: Positionsbetrag, wenn angegeben, sonst null. lieferant: Firmenname des Lieferanten. \
datum: Belegdatum im Format JJJJ-MM-TT, leer wenn unbekannt. Positionen ohne Preis, Frachthinweise, \
Zwischensummen und Summen weglassen.";

#[derive(Debug, Deserialize)]
struct Beleg {
    lieferant: String,
    datum: String,
    positionen: Vec<Pos>,
}

#[derive(Debug, Deserialize)]
struct Pos {
    bezeichnung: String,
    menge: f64,
    einheit: String,
    einzelpreis_netto: f64,
    gesamtpreis: Option<f64>,
}

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "lieferant": {"type": "string"},
            "datum": {"type": "string"},
            "positionen": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "bezeichnung": {"type": "string"},
                    "menge": {"type": "number"},
                    "einheit": {"type": "string"},
                    "einzelpreis_netto": {"type": "number"},
                    "gesamtpreis": {"type": ["number", "null"]}
                },
                "required": ["bezeichnung", "menge", "einheit", "einzelpreis_netto", "gesamtpreis"]
            }}
        },
        "required": ["lieferant", "datum", "positionen"]
    })
}

/// Prüft die Positionen: Preis und Menge positiv, und wo ein Positionsbetrag steht, muss
/// Menge × Einzelpreis dazu passen (sonst hat das Modell die Spalten verwechselt).
fn plausible(positionen: Vec<Pos>) -> Vec<EkRow> {
    let mut out: Vec<EkRow> = Vec::new();
    for p in positionen {
        let ok = p.einzelpreis_netto.is_finite()
            && p.einzelpreis_netto > 0.0
            && p.menge.is_finite()
            && p.menge > 0.0
            && !p.bezeichnung.trim().is_empty()
            && p.gesamtpreis.is_none_or(|gp| {
                (p.menge * p.einzelpreis_netto - gp).abs() <= (gp.abs() * 0.01).max(0.05)
            });
        if !ok {
            continue;
        }
        let row = EkRow {
            bezeichnung: p.bezeichnung.trim().to_string(),
            menge: Some(format!("{}", p.menge)),
            einheit: Some(p.einheit.trim().to_string()).filter(|e| !e.is_empty()),
            ep: format!("{}", (p.einzelpreis_netto * 10_000.0).round() / 10_000.0),
        };
        // Überlappende Chunks können eine Position doppelt liefern.
        if !out
            .iter()
            .any(|r| r.bezeichnung == row.bezeichnung && r.ep == row.ep)
        {
            out.push(row);
        }
    }
    out
}

fn belegdatum(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .ok()
        .filter(|d| (1990..=2100).contains(&chrono::Datelike::year(d)))
}

async fn read_one(llm: &LlmClient, doc: &Candidate) -> Result<Beleg, LlmError> {
    let messages = [ChatMessage::system(SYSTEM), ChatMessage::user(&doc.text)];
    let options = ChatOptions {
        temperature: 0.0,
        max_tokens: Some(4000),
    };
    llm.chat_json(&messages, &options, "beleg", &schema()).await
}

/// Endlosschleife bis `stop`: Beleg holen, auslesen, speichern.
pub async fn run(pool: db::PgPool, llm_url: String, stop: CancellationToken) {
    let llm = match LlmClient::new(llm_url, LLM_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "LLM-Client nicht erzeugbar, EK-Preise werden nicht gelesen");
            return;
        }
    };
    tracing::info!("EK-Preise: Lieferantenbelege werden im Hintergrund gelesen");
    while !stop.is_cancelled() {
        let pause = match step(&pool, &llm).await {
            Ok(true) => Duration::from_secs(1),
            Ok(false) => Duration::from_secs(600),
            Err(e) => {
                tracing::warn!(error = %e, "EK-Preise: Pause nach Fehler");
                Duration::from_secs(300)
            }
        };
        tokio::select! {
            () = stop.cancelled() => break,
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// Ein Beleg. `Ok(false)`: nichts mehr zu tun. `Err`: LLM oder Datenbank nicht erreichbar,
/// der Beleg bleibt offen.
async fn step(pool: &db::PgPool, llm: &LlmClient) -> anyhow::Result<bool> {
    let Some(doc) = db::ek::next_candidate(pool, MAX_CHARS).await? else {
        return Ok(false);
    };
    if doc.text.trim().len() < 50 {
        db::ek::store(pool, &doc, "", None, &[], Some("kein Text")).await?;
        return Ok(true);
    }
    let started = std::time::Instant::now();
    match read_one(llm, &doc).await {
        Ok(beleg) => {
            let rows = plausible(beleg.positionen);
            let datum = belegdatum(&beleg.datum);
            db::ek::store(pool, &doc, beleg.lieferant.trim(), datum, &rows, None).await?;
            tracing::info!(beleg = %doc.rel_path, preise = rows.len(), dauer_s = started.elapsed().as_secs(), "EK-Preise gelesen");
        }
        // Kaputtes oder abgeschnittenes JSON (sehr lange Preisliste): Beleg als gelesen merken.
        Err(e @ (LlmError::Deserialize(_) | LlmError::InvalidResponse(_))) => {
            db::ek::store(pool, &doc, "", None, &[], Some(&e.to_string())).await?;
            tracing::info!(beleg = %doc.rel_path, error = %e, "EK-Preise: Beleg nicht lesbar");
        }
        Err(e) => return Err(e.into()),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(b: &str, menge: f64, ep: f64, gp: Option<f64>) -> Pos {
        Pos {
            bezeichnung: b.into(),
            menge,
            einheit: "St".into(),
            einzelpreis_netto: ep,
            gesamtpreis: gp,
        }
    }

    #[test]
    fn keeps_only_consistent_positions() {
        let rows = plausible(vec![
            pos("Mauerscheibe 105x65x100", 12.0, 108.75, Some(1305.0)),
            pos("Massivstück", 1.0, 1850.0, Some(83.25)),
            pos("Prunus avium", 2.0, 155.0, None),
            pos("Prunus avium", 2.0, 155.0, None),
            pos("Ohne Preis", 1.0, 0.0, None),
            pos(" ", 1.0, 5.0, None),
        ]);
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.bezeichnung.as_str(), r.ep.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("Mauerscheibe 105x65x100", "108.75"),
                ("Prunus avium", "155")
            ]
        );
    }

    #[test]
    fn dates() {
        assert_eq!(
            belegdatum("2025-04-24"),
            NaiveDate::from_ymd_opt(2025, 4, 24)
        );
        assert_eq!(belegdatum("0000-00-00"), None);
        assert_eq!(belegdatum(""), None);
    }
}
