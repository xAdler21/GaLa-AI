//! Preise aus eigenen Angeboten im PDF-Format (Druck aus der AVA: „Ordnungszahl
//! Leistungsbeschreibung Menge Einheitspreis Gesamtbetrag“). Gelesen wird der fertige
//! Dokumenttext (Textebene und OCR), damit auch gescannte Angebote zählen.
//!
//! Aufbau einer Position im Druck:
//! ```text
//! 02.01 Auf- und Abtrag, für Belagsflächen      ← OZ + Kurztext
//! Boden im Bereich des Baufeldes lösen …        ← Langtext
//! 120,00 m2 25,50 3.060,00                       ← Menge Einheit EP GP (GP fehlt bei Bedarf)
//! ```
//! Titel haben eine OZ, aber keine Mengenzeile vor der nächsten OZ und fallen dadurch weg.

use lv_model::Unit;

use crate::extract::{PRICE_LANGTEXT_MAX, PriceRow};

/// Nur eigene Angebote: Dateiname enthält „Angebot“, Lieferantenangebote sind ausgenommen.
pub fn is_offer_path(rel_path: &str) -> bool {
    let lower = rel_path.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.ends_with(".pdf") && name.contains("angebot") && !lower.contains("lieferant")
}

/// Setzt den Dokumenttext aus den gespeicherten Chunks (nach `ord` sortiert) wieder zusammen.
/// Aufeinanderfolgende Chunks einer Seite überlappen um einen Absatz; der wird übersprungen.
pub fn text_from_chunks(chunks: &[(Option<i32>, String)]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev: Option<(Option<i32>, &str)> = None;
    for (page, text) in chunks {
        let mut units: Vec<&str> = text.split("\n\n").collect();
        if let Some((p, last)) = prev
            && p == *page
            && units.first() == Some(&last)
        {
            units.remove(0);
        }
        prev = text.rsplit("\n\n").next().map(|l| (*page, l));
        out.extend(units);
    }
    out.join("\n\n")
}

/// Liest alle Positionen mit Einheitspreis aus dem Text eines Angebots.
pub fn parse_offer(text: &str, lv_name: &str) -> Vec<PriceRow> {
    if !text.contains("Einheitspreis") {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut cur: Option<Open> = None;
    // Zwischen „Übertrag“ am Seitenende und dem Tabellenkopf der nächsten Seite stehen Fußzeile
    // und Briefkopf; die gehören nicht in den Langtext.
    let mut skipping = false;
    let mut after_header = false;

    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("Ordnungszahl") {
            skipping = false;
            after_header = true;
            continue;
        }
        if line.starts_with("Übertrag") {
            if !after_header {
                skipping = true;
            }
            continue;
        }
        after_header = false;
        if let Some((oz, rest)) = oz_line(line) {
            skipping = false;
            cur = Some(Open::new(oz, rest));
            continue;
        }
        if skipping || line.starts_with("Summe") {
            continue;
        }
        let Some(open) = cur.as_mut() else { continue };
        if let Some(q) = qty_line(line) {
            let oz = open.oz.clone();
            if let Some(row) = cur.take().and_then(|o| o.finish(q, lv_name)) {
                rows.push(row);
            }
            // Pflanzlisten: unter einer OZ folgen weitere Artikel mit eigener Mengenzeile.
            cur = Some(Open::new(&oz, ""));
        } else {
            open.push(line);
        }
    }
    rows
}

struct Open {
    oz: String,
    kurztext: String,
    langtext: Vec<String>,
    art: &'static str,
}

impl Open {
    fn new(oz: &str, rest: &str) -> Self {
        // „***Bedarfsposition ohne Ges.-Betrag“: Kennzeichen statt Kurztext, der folgt danach.
        let (art, kurztext) = match rest.strip_prefix("***") {
            Some(mark) => {
                let m = mark.to_lowercase();
                let art = if m.contains("bedarf") {
                    "bedarf"
                } else if m.contains("wahl") || m.contains("alternativ") {
                    "alternativ"
                } else if m.contains("zulage") {
                    "zulage"
                } else {
                    "normal"
                };
                (art, String::new())
            }
            None => ("normal", rest.to_string()),
        };
        Self {
            oz: oz.to_string(),
            kurztext,
            langtext: Vec::new(),
            art,
        }
    }

    fn push(&mut self, line: &str) {
        if self.kurztext.is_empty() && line.starts_with("***") {
            self.art = Self::new(&self.oz, line).art;
        } else if self.kurztext.is_empty() {
            self.kurztext = line.to_string();
        } else {
            self.langtext.push(line.to_string());
        }
    }

    fn finish(self, q: Qty, lv_name: &str) -> Option<PriceRow> {
        if self.kurztext.is_empty() || q.ep <= 0.0 {
            return None;
        }
        // Mit Gesamtbetrag: Menge × EP muss passen, sonst ist die Zeile falsch gelesen (OCR).
        if let (Some(menge), Some(gp)) = (q.menge_f, q.gp)
            && (menge * q.ep - gp).abs() > (gp.abs() * 0.01).max(0.05)
        {
            return None;
        }
        let langtext: String = self
            .langtext
            .join("\n")
            .chars()
            .take(PRICE_LANGTEXT_MAX)
            .collect();
        Some(PriceRow {
            phase: 84,
            lv_name: lv_name.to_string(),
            oz: self.oz,
            kurztext: self.kurztext,
            langtext,
            menge: q.menge,
            einheit: Some(q.einheit),
            ep: q.ep_s,
            art: self.art.to_string(),
        })
    }
}

/// OZ am Zeilenanfang (mindestens zwei Ebenen wie `01.02` oder `1.2.30`) und Text danach.
/// Einstufige Nummern wie „1 Stk Linde“ im Langtext sind damit keine Position.
fn oz_line(line: &str) -> Option<(&str, &str)> {
    let (first, rest) = line.split_once(char::is_whitespace)?;
    let oz = first.strip_suffix('.').unwrap_or(first);
    let segs: Vec<&str> = oz.split('.').collect();
    let ok = segs.len() >= 2
        && segs
            .iter()
            .all(|s| (1..=4).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()));
    let rest = rest.trim();
    let starts_text = rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '*');
    (ok && starts_text).then_some((oz, rest))
}

struct Qty {
    menge: Option<String>,
    menge_f: Option<f64>,
    einheit: String,
    ep: f64,
    ep_s: String,
    gp: Option<f64>,
}

/// Mengenzeile: `120,00 m2 25,50 [3.060,00]` oder `pauschal 1.000,00` / `psch 700,00 [700,00]`.
fn qty_line(line: &str) -> Option<Qty> {
    let toks: Vec<&str> = line
        .split_whitespace()
        .filter(|t| *t != "€")
        .skip_while(|t| t.eq_ignore_ascii_case("ca."))
        .collect();
    let (menge, unit, prices) = match toks.as_slice() {
        [m, u, p @ ..] if quantity(m).is_some() && de_num(u).is_none() => (Some(*m), *u, p),
        [u, p @ ..] if de_num(u).is_none() && is_lump(u) => (None, *u, p),
        _ => return None,
    };
    let (ep, gp) = match prices {
        [ep] => (*ep, None),
        [ep, gp] => (*ep, Some(*gp)),
        _ => return None,
    };
    let (ep_s, ep) = de_num(ep)?;
    let gp = match gp {
        Some(g) => Some(de_num(g)?.1),
        None => None,
    };
    let (menge, menge_f) = match menge {
        Some(m) => {
            let (s, f) = quantity(m)?;
            (Some(s), Some(f))
        }
        None => (Some("1".to_string()), Some(1.0)),
    };
    // Wie in GAEB-Dateien schreiben (m² → m2, Stück → St); „Ifm“ ist ein OCR-Lesefehler für lfm.
    let einheit = Unit::parse(if unit == "Ifm" { "lfm" } else { unit })
        .map_or_else(|| unit.to_string(), |u| u.symbol().to_string());
    Some(Qty {
        menge,
        menge_f,
        einheit,
        ep,
        ep_s,
        gp,
    })
}

fn is_lump(u: &str) -> bool {
    Unit::parse(u) == Some(Unit::Psch)
}

/// Menge: deutsche Zahl mit Nachkommastellen oder ganze Stückzahl (`290`).
fn quantity(s: &str) -> Option<(String, f64)> {
    if (1..=6).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) {
        return Some((s.to_string(), s.parse().ok()?));
    }
    de_num(s)
}

/// Deutsche Zahl mit 2–3 Nachkommastellen (`1.234,56`) → (`"1234.56"`, Wert).
fn de_num(s: &str) -> Option<(String, f64)> {
    let (int, frac) = s.split_once(',')?;
    if !(2..=3).contains(&frac.len()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let groups: Vec<&str> = int.split('.').collect();
    let ok = !groups[0].is_empty()
        && groups[0].len() <= if groups.len() > 1 { 3 } else { 9 }
        && groups[1..].iter().all(|g| g.len() == 3)
        && groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return None;
    }
    let plain = format!("{}.{frac}", groups.concat());
    let v = plain.parse().ok()?;
    Some((plain, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFER: &str = "Angebot

Ordnungszahl Leistungsbeschreibung Menge Einheitspreis (€) Gesamtbetrag (€)

01 Vorarbeiten

01.01 Baustelleneinrichtung
Einmaliges Einrichten und Räumen der Baustelle.

1,00 psch 700,00 700,00

01.02 ***Bedarfsposition ohne Ges.-Betrag

Verkehrsrechtliche Anordnung einholen,
vorhalten für die Dauer der Baumaßnahme.

Übertrag: 700,00

Mitglied im Verband ARTINGER GmbH & Co. KG
IBAN: DE00 0000

Angebot
Seite 2 / 33

Ordnungszahl Leistungsbeschreibung Menge Einheitspreis (€) Gesamtbetrag (€)

Übertrag: 700,00

1,00 psch 530,00

Summe 01 Vorarbeiten 700,00

02 Erdarbeiten

02.01 Auf- und Abtrag
lt. Baubeschreibung:
1 Stk Winter-Linde 1. Wuchsordnung
1 m3 = 1,50 to lose Masse

1.200,00 m² 25,50 30.600,00

02.02 Falsch gelesen
10,00 m2 5,00 99,00

02.03 Pauschale
pauschal 1.000,00

03.01 Gräser
Miscanthus giganteus
Tb
ca. 290 Stück 5,70 1.653,00
Phragmites australis
380 Stück 2,45 931,00
***Alternativposition
Typha latifolia
380 Stück (2,95 ) (1.121,00 )
";

    #[test]
    fn reads_positions_across_page_break() {
        let rows = parse_offer(OFFER, "Angebot Test");
        let got: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.oz.as_str(),
                    r.kurztext.as_str(),
                    r.menge.as_deref(),
                    r.einheit.as_deref(),
                    r.ep.as_str(),
                    r.art.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "01.01",
                    "Baustelleneinrichtung",
                    Some("1.00"),
                    Some("psch"),
                    "700.00",
                    "normal"
                ),
                (
                    "01.02",
                    "Verkehrsrechtliche Anordnung einholen,",
                    Some("1.00"),
                    Some("psch"),
                    "530.00",
                    "bedarf"
                ),
                (
                    "02.01",
                    "Auf- und Abtrag",
                    Some("1200.00"),
                    Some("m2"),
                    "25.50",
                    "normal"
                ),
                (
                    "02.03",
                    "Pauschale",
                    Some("1"),
                    Some("psch"),
                    "1000.00",
                    "normal"
                ),
                ("03.01", "Gräser", Some("290"), Some("St"), "5.70", "normal"),
                (
                    "03.01",
                    "Phragmites australis",
                    Some("380"),
                    Some("St"),
                    "2.45",
                    "normal"
                ),
            ]
        );
        // Fußzeile und Briefkopf der Folgeseite gehören nicht in den Langtext.
        assert_eq!(rows[1].langtext, "vorhalten für die Dauer der Baumaßnahme.");
        assert!(rows[2].langtext.contains("1 Stk Winter-Linde"));
    }

    #[test]
    fn no_header_no_prices() {
        assert!(parse_offer("01.01 Pflaster\n10,00 m2 5,00 50,00", "x").is_empty());
    }

    #[test]
    fn offer_paths() {
        assert!(is_offer_path(
            "2025/MADER/01_Ausführung/Angebot Fa. Artinger - 28.02.25.pdf"
        ));
        assert!(!is_offer_path(
            "2020/X/04_Lieferantendokumentation/Angebote/Baustoff Angebot.pdf"
        ));
        assert!(!is_offer_path("2020/Angebote/LV.pdf"));
        assert!(!is_offer_path("2020/X/Angebot.d84"));
    }

    #[test]
    fn german_numbers() {
        assert_eq!(de_num("1.234,56").unwrap().0, "1234.56");
        assert_eq!(de_num("12,500").unwrap().0, "12.500");
        assert!(de_num("1234").is_none());
        assert!(de_num("12.34,56").is_none());
        assert!(de_num("m2").is_none());
    }

    #[test]
    fn chunks_overlap_is_dropped() {
        let chunks = vec![
            (Some(1), "A\n\nB".to_string()),
            (Some(1), "B\n\nC".to_string()),
            (Some(2), "C\n\nD".to_string()),
        ];
        assert_eq!(text_from_chunks(&chunks), "A\n\nB\n\nC\n\nC\n\nD");
    }
}
