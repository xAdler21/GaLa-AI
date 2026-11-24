//! Preise aus echten PDF-Angeboten (Kundendaten, nicht im Repo). Läuft nur mit
//! `GALA_PDF_OFFER_DUMP` = JSON-Zeilen `{"path": …, "chunks": [[seite, text], …]}` (Export aus
//! der Tabelle `chunk`) und zeigt, wie viele Preise übernommen würden.

use ingest::pdf_prices::{is_offer_path, parse_offer, text_from_chunks};

#[test]
fn offers_in_dump_yield_prices() {
    let Some(file) = std::env::var_os("GALA_PDF_OFFER_DUMP") else {
        return;
    };
    let dump = std::fs::read_to_string(file).unwrap();
    let (mut docs, mut with_header, mut with_prices, mut rows) = (0, 0, 0, 0);
    let mut missed = Vec::new();
    for line in dump.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let path = v["path"].as_str().unwrap();
        assert!(is_offer_path(path), "{path}");
        let chunks: Vec<(Option<i32>, String)> = v["chunks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        (
                            c[0].as_i64().map(|p| p as i32),
                            c[1].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        docs += 1;
        let text = text_from_chunks(&chunks);
        if !text.contains("Einheitspreis") {
            continue;
        }
        with_header += 1;
        let r = parse_offer(&text, path);
        if r.is_empty() {
            missed.push(path.to_string());
            continue;
        }
        with_prices += 1;
        rows += r.len();
        if std::env::var_os("GALA_PDF_OFFER_SHOW").is_some() {
            println!("== {path}");
            for p in &r {
                println!(
                    "  {} | {} | {:?} {:?} | EP {} | {}",
                    p.oz, p.kurztext, p.menge, p.einheit, p.ep, p.art
                );
            }
        }
    }
    println!(
        "{docs} PDF-Angebote, {with_header} mit Preistabelle, {with_prices} mit Preisen, {rows} Preise"
    );
    for m in &missed {
        println!("  ohne Preise: {m}");
    }
}
