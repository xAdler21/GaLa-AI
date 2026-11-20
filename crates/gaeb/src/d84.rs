//! Angebotsabgabe D84 (GAEB 90) aus der Original-D83.
//!
//! Das Format folgt den D84-Dateien, die der Betrieb bisher abgegeben hat:
//! Kopf `00` (Phase 84), `01`–`03` und `08` aus der D83, `04` Bieter, je bepreister Position
//! ein Satz `23` (OZ wie in der D83, EP, GP), je LV-Bereich eine Summe `32`, am Ende `99`
//! mit Netto, Steuersatz, Brutto und Anzahl der Preissätze. Laufende Nummer in Sp. 75–80,
//! Zeilenende CRLF, Zeichensatz CP850.

use std::collections::HashMap;

use lv_model::{Decimal, Lv, Oz, RoundingStrategy};

use crate::GaebError;
use crate::d90::{Mask, Rec, decode_d90, encode_cp850};

/// Umsatzsteuer in Satz 99, in Hundertstel Prozent wie im Format (`1900` = 19 %).
// shortcut: fester Satz 19 %, bei abweichendem Satz Parameter einführen.
const VAT_BP: i64 = 1900;

/// Ergebnis des D84-Exports.
#[derive(Debug, Clone)]
pub struct D84Export {
    /// Fertige Datei (CP850, CRLF).
    pub bytes: Vec<u8>,
    /// Positionen ohne Einheitspreis; für sie gibt es keinen Satz `23`.
    pub missing_prices: Vec<Oz>,
    /// Angebotssumme netto, wie in Satz 99.
    pub total: Decimal,
}

/// Wert als Ganzzahl in `scale` Nachkommastellen (kaufmännisch gerundet).
fn scaled(value: Decimal, scale: u32) -> i128 {
    let mut d = value.round_dp_with_strategy(scale, RoundingStrategy::MidpointAwayFromZero);
    d.rescale(scale);
    d.mantissa()
}

/// Rechtsbündig mit Nullen; negative Beträge kann das Feld nicht darstellen.
// shortcut: negative Preise (Nachlass-Positionen) werden zu 0, bei Bedarf Vorzeichen-Spalte klären.
fn num(value: i128, width: usize) -> String {
    format!("{:0>width$}", value.max(0))
}

struct Out {
    lines: Vec<String>,
}

impl Out {
    /// Setzt Felder (1-basierte Startspalte) in eine leere Zeile und hängt sie mit Nummer an.
    fn push(&mut self, kind: &str, fields: &[(usize, &str)]) {
        let mut chars = vec![' '; 74];
        for (i, c) in kind.chars().enumerate() {
            chars[i] = c;
        }
        for &(col, text) in fields {
            for (i, c) in text.chars().enumerate() {
                if let Some(slot) = chars.get_mut(col - 1 + i) {
                    *slot = c;
                }
            }
        }
        self.push_content(chars.into_iter().collect());
    }

    /// Hängt Sp. 1–74 unverändert an.
    fn push_content(&mut self, content: String) {
        let n = self.lines.len() + 1;
        self.lines.push(format!("{content}{n:06}"));
    }
}

/// Erzeugt die D84 aus der Original-D83 und den Preisen des bepreisten LVs (Zuordnung über
/// die OZ). `bidder` kommt in Satz 04; ohne Angabe bleibt ein vorhandener Satz 04 stehen.
pub fn write_d84(original: &[u8], lv: &Lv, bidder: Option<&str>) -> Result<D84Export, GaebError> {
    let recs = Rec::split(&decode_d90(original));
    let header = recs
        .iter()
        .find(|r| r.kind() == "00")
        .ok_or(GaebError::Missing("Satz 00 (Kopf)"))?;
    let mask =
        Mask::parse(&header.cols(63, 71)).ok_or(GaebError::Missing("OZ-Maske in Satz 00"))?;

    let prices: HashMap<&Oz, _> = lv
        .positions()
        .filter_map(|p| Some((p.oz.value.as_ref()?, p)))
        .collect();

    let mut out = Out { lines: Vec::new() };
    let mut missing_prices = Vec::new();
    // Offene LV-Bereiche: (Tiefe, OZ-Feld Sp. 3–11, Summe in Cent).
    let mut titles: Vec<(usize, String, i128)> = Vec::new();
    let mut total: i128 = 0;
    let mut price_lines = 0usize;
    let mut bidder_written = false;

    let close_titles = |out: &mut Out, titles: &mut Vec<(usize, String, i128)>, depth: usize| {
        while titles.last().is_some_and(|(d, _, _)| *d >= depth) {
            let (_, field, sum) = titles.pop().expect("geprüft");
            out.push("32", &[(3, &field), (13, &num(sum, 12))]);
        }
    };
    let write_bidder = |out: &mut Out, written: &mut bool| {
        if let Some(name) = bidder.filter(|_| !*written) {
            out.push("04", &[(3, name)]);
            *written = true;
        }
    };

    for r in &recs {
        let content = r.cols(1, 74);
        match r.kind().as_str() {
            "00" => {
                let mut c: Vec<char> = content.chars().collect();
                c[10] = '8';
                c[11] = '4';
                c[12] = ' ';
                out.push_content(c.into_iter().collect());
            }
            "01" => {
                let mut c: Vec<char> = content.chars().collect();
                c[71] = 'X';
                out.push_content(c.into_iter().collect());
            }
            "02" | "03" => out.push_content(content),
            "04" => {
                if bidder.is_some() {
                    write_bidder(&mut out, &mut bidder_written);
                } else {
                    out.push_content(content);
                }
            }
            "08" => {
                write_bidder(&mut out, &mut bidder_written);
                out.push_content(content);
            }
            "11" => {
                let field = r.cols(3, 11);
                let depth = mask.oz(&field).as_ref().map_or(1, Oz::depth);
                close_titles(&mut out, &mut titles, depth);
                titles.push((depth, field, 0));
            }
            "31" => {
                let depth = mask.oz(&r.cols(3, 11)).as_ref().map_or(1, Oz::depth);
                close_titles(&mut out, &mut titles, depth);
            }
            "21" => {
                let field = r.cols(3, 11);
                let Some(oz) = mask.oz(&field) else { continue };
                // Pauschalen tragen manchmal nur einen GP (Sp. 13–23 bleiben dann leer).
                let Some(p) = prices
                    .get(&oz)
                    .filter(|p| p.ep.value.is_some() || p.gp.value.is_some())
                else {
                    missing_prices.push(oz);
                    continue;
                };
                let mut fields = vec![(3, field.clone())];
                if let Some(ep) = p.ep.value {
                    let ep_milli = scaled(ep, 3);
                    let third = match ep_milli % 10 {
                        0 => " ".to_string(),
                        d => d.to_string(),
                    };
                    fields.push((13, num(ep_milli / 10, 10)));
                    fields.push((23, third));
                }
                // Der GP steht auch bei Bedarfs- und Wahlpositionen im Satz (so die bisherigen
                // Abgaben), zählt aber nur bei den übrigen in Bereichssummen und Netto.
                // Wie im X84: ohne Menge (Pauschale) ist der GP der EP.
                let gp =
                    p.gp.value
                        .or_else(|| p.computed_gp())
                        .or(if p.art.counts_towards_total() {
                            p.ep.value
                        } else {
                            None
                        });
                let cents = gp.map_or(0, |gp| scaled(gp, 2));
                fields.push((25, num(cents, 12)));
                if p.art.counts_towards_total() {
                    total += cents;
                    for t in &mut titles {
                        t.2 += cents;
                    }
                }
                let fields: Vec<(usize, &str)> =
                    fields.iter().map(|(c, s)| (*c, s.as_str())).collect();
                out.push("23", &fields);
                price_lines += 1;
            }
            _ => {}
        }
    }
    close_titles(&mut out, &mut titles, 0);
    let gross = (total * i128::from(10_000 + VAT_BP) + 5_000) / 10_000;
    out.push(
        "99",
        &[
            (3, &num(total, 12)),
            (22, &VAT_BP.to_string()),
            (26, &num(gross, 12)),
            (70, &num(price_lines as i128, 5)),
        ],
    );

    let mut text = out.lines.join("\r\n");
    text.push_str("\r\n");
    Ok(D84Export {
        bytes: encode_cp850(&text),
        missing_prices,
        total: Decimal::from_i128_with_scale(total, 2),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import;
    use rust_decimal_macros::dec;

    /// Kleine D83 im Format des Archivs: ein Bereich mit Unterbereich, drei Positionen
    /// (eine Bedarfsposition), Texte und STLB-Bezug.
    fn d83() -> Vec<u8> {
        let lines = [
            "00        83L23B0424                                          1122PPPPI90 ",
            "01Landschaftsbauarbeiten                  30.08.23                        ",
            "02Studierendenhaus                                                        ",
            "08EURO  EURO                                                              ",
            "1101       N    Bereich                                                   ",
            "12Dachbegrünung                                                           ",
            "110101     N    Abschnitt                                                 ",
            "12KG 576                                                                  ",
            "2101010001 NNN         00000005000m      X                                ",
            "25Baustelle einrichten                                                    ",
            "79STLB-Bau  VJ 2023 VM 04 LB 001 TLG 5335                                 ",
            "2101010002 NNN         00000025000m2                                      ",
            "25Substrat liefern                                                        ",
            "2101010003 NEN         00000010000St                                      ",
            "25Bedarf: Gully                                                           ",
            "310101                                                                    ",
            "3101                                                                      ",
            "99                                                                        ",
        ];
        let mut s = String::new();
        for (i, l) in lines.iter().enumerate() {
            s.push_str(&format!("{l:<74}{:06}\r\n", i + 1));
        }
        encode_cp850(&s)
    }

    #[test]
    fn writes_prices_sums_and_trailer() {
        let mut lv = import(&d83()).unwrap().lv;
        let ozs: Vec<Oz> = lv.positions().filter_map(|p| p.oz.value.clone()).collect();
        assert_eq!(ozs.len(), 3);
        for (oz, ep) in ozs.iter().zip([dec!(300), dec!(12.345), dec!(80)]) {
            lv.position_mut(oz).unwrap().ep.value = Some(ep);
        }
        let out = write_d84(&d83(), &lv, Some("Muster GaLaBau GmbH")).unwrap();
        let text = decode_d90(&out.bytes);
        let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert!(lines.iter().all(|l| l.chars().count() == 80));
        let body: Vec<&str> = lines.iter().map(|l| l[..74].trim_end()).collect();
        // 5 m × 300 = 1500,00; 25 m² × 12,345 = 308,625 → 308,63; Bedarf mit GP, aber nicht in der Summe.
        assert_eq!(
            body,
            [
                "00        84 23B0424                                          1122PPPPI90",
                "01Landschaftsbauarbeiten                  30.08.23                     X",
                "02Studierendenhaus",
                "04Muster GaLaBau GmbH",
                "08EURO  EURO",
                "2301010001  0000030000  000000150000",
                "2301010002  00000012345 000000030863",
                "2301010003  0000008000  000000080000",
                "320101      000000180863",
                "3201        000000180863",
                "99000000180863       1900000000215227                                00003",
            ]
        );
        assert_eq!(lines[3].get(74..), Some("000004"));
        assert_eq!(out.total, dec!(1808.63));
        assert!(out.missing_prices.is_empty());

        // Rücklesen: Preise kommen wieder an.
        let back = import(&out.bytes).unwrap().lv;
        let eps: Vec<_> = back.positions().map(|p| p.ep.value).collect();
        assert_eq!(eps, [Some(dec!(300)), Some(dec!(12.345)), Some(dec!(80))]);
    }

    #[test]
    fn reports_missing_prices() {
        let lv = import(&d83()).unwrap().lv;
        let out = write_d84(&d83(), &lv, None).unwrap();
        assert_eq!(out.missing_prices.len(), 3);
        assert_eq!(out.total, dec!(0));
    }
}
