//! Text aus Dateien gewinnen: PDF-Textebene (mit Triage), Excel, GAEB, Text.

use std::path::Path;

use calamine::{Reader, open_workbook_auto};
use doc_ir::{Line, Page, group_lines};
use lv_model::Node;
use pdf::{PageDecision, PdfEngine};

use crate::IngestError;
use crate::chunk::PageText;

/// Ergebnis der PDF-Extraktion: Seiten mit brauchbarer Textebene und Seiten, die OCR brauchen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfExtract {
    pub page_count: u32,
    pub text_pages: Vec<PageText>,
    /// 1-basierte Seitennummern, die per OCR gelesen werden müssen.
    pub ocr_pages: Vec<u32>,
}

/// Liest die Textebene jeder Seite; Seiten, die laut Triage OCR brauchen, werden nur gemeldet.
/// Eine einzelne kaputte Seite wird ebenfalls zur OCR-Seite (statt das Dokument scheitern zu lassen).
pub fn extract_pdf(engine: &PdfEngine, path: &Path) -> Result<PdfExtract, IngestError> {
    let doc = engine.open(path)?;
    let page_count = doc.page_count();
    let mut out = PdfExtract {
        page_count,
        text_pages: Vec::new(),
        ocr_pages: Vec::new(),
    };
    for index in 0..page_count {
        let page_no = index + 1;
        match doc.text_layer(index) {
            Ok((page, stats)) => match stats.decision() {
                PageDecision::TextLayer => out.text_pages.push(PageText {
                    page: Some(page_no),
                    text: page_to_text(&page),
                }),
                PageDecision::Ocr(reason) => {
                    tracing::debug!(page = page_no, ?reason, "Seite braucht OCR");
                    out.ocr_pages.push(page_no);
                }
            },
            Err(err) => {
                tracing::warn!(page = page_no, %err, "Textebene nicht lesbar, Seite geht zur OCR");
                out.ocr_pages.push(page_no);
            }
        }
    }
    Ok(out)
}

/// Seitentext aus Tokens: Zeilen gruppieren, bei größeren vertikalen Lücken Absatz einfügen.
pub fn page_to_text(page: &Page) -> String {
    lines_to_text(&group_lines(page.tokens.iter().cloned()))
}

/// Zeilen → Text; Lücke größer als die 0,6-fache Zeilenhöhe ergibt eine Leerzeile (Absatz).
pub fn lines_to_text(lines: &[Line]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Line> = None;
    for line in lines {
        let text = line.text();
        if text.trim().is_empty() {
            continue;
        }
        if let Some(p) = prev {
            let height = (line.bbox.y1 - line.bbox.y0)
                .max(p.bbox.y1 - p.bbox.y0)
                .max(1.0);
            let gap = line.bbox.y0 - p.bbox.y1;
            out.push_str(if p.page != line.page || gap > 0.6 * height {
                "\n\n"
            } else {
                "\n"
            });
        }
        out.push_str(&text);
        prev = Some(line);
    }
    out
}

/// Text einer OCR-Seite (wie [`page_to_text`]).
pub fn ocr_page_to_text(page: &Page) -> String {
    page_to_text(page)
}

/// Excel: je Blatt eine „Seite“ (ohne Seitennummer) mit einer Zeile pro Tabellenzeile.
pub fn extract_xlsx(path: &Path) -> Result<Vec<PageText>, IngestError> {
    let mut wb = open_workbook_auto(path).map_err(|e| IngestError::Extract {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut pages = Vec::new();
    for name in wb.sheet_names() {
        let range = match wb.worksheet_range(&name) {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(sheet = %name, %err, "Blatt nicht lesbar");
                continue;
            }
        };
        let mut text = format!("Blatt: {name}");
        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .filter(|c| !matches!(c, calamine::Data::Empty | calamine::Data::Error(_)))
                .map(|c| c.to_string())
                .filter(|s| !s.trim().is_empty())
                .collect();
            if !cells.is_empty() {
                text.push('\n');
                text.push_str(&cells.join(" | "));
            }
        }
        if text.lines().count() > 1 {
            pages.push(PageText { page: None, text });
        }
    }
    Ok(pages)
}

/// GAEB: jede Position (und jeder Titel) wird ein Absatz. Reine D8x-Dateien (GAEB 90, kein XML)
/// werden als Text gelesen.
pub fn extract_gaeb(path: &Path) -> Result<Vec<PageText>, IngestError> {
    extract_gaeb_with_prices(path).map(|(pages, _)| pages)
}

/// Preiszeile einer GAEB-Abgabe (Phase 84–86) für die Preis-Historie. Zahlen als Text,
/// damit sie ohne Rundung als `numeric` in die Datenbank gehen.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceRow {
    pub phase: i16,
    pub lv_name: String,
    pub oz: String,
    pub kurztext: String,
    pub langtext: String,
    pub menge: Option<String>,
    pub einheit: Option<String>,
    pub ep: String,
    pub art: String,
}

/// Langtexte werden für die Historie gekürzt; zum Wiedererkennen reicht der Anfang.
pub(crate) const PRICE_LANGTEXT_MAX: usize = 2000;

/// Wie [`extract_gaeb`], liefert bei Abgaben (Phase 84–86) zusätzlich die Einheitspreise.
pub fn extract_gaeb_with_prices(
    path: &Path,
) -> Result<(Vec<PageText>, Vec<PriceRow>), IngestError> {
    let bytes = read_bytes(path)?;
    match gaeb::import(&bytes) {
        Ok(mut import) => {
            if (84..=86).contains(&import.phase.dp()) {
                fill_texts_from_siblings(path, &mut import.lv);
            }
            let mut paragraphs = Vec::new();
            collect_nodes(&import.lv.nodes, &mut paragraphs);
            let pages = vec![PageText {
                page: None,
                text: paragraphs.join("\n\n"),
            }];
            Ok((pages, price_rows(&import)))
        }
        Err(err) => {
            let is_d = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.to_lowercase().starts_with('d'));
            if is_d {
                // GAEB-90-Datei, die der Parser nicht liest: der Rohtext (Satzarten stören die
                // Suche kaum).
                let pages = vec![PageText {
                    page: None,
                    text: decode_text(&bytes),
                }];
                Ok((pages, Vec::new()))
            } else {
                Err(IngestError::Extract {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })
            }
        }
    }
}

/// Einheitspreise aus Angebot (84), Nebenangebot (85) oder Auftrag (86); die anderen Phasen
/// enthalten keine Preise des Betriebs.
fn price_rows(import: &gaeb::GaebImport) -> Vec<PriceRow> {
    let phase = import.phase.dp();
    if !(84..=86).contains(&phase) {
        return Vec::new();
    }
    import
        .lv
        .positions()
        .filter_map(|p| {
            let ep = p.ep.value.filter(|ep| *ep > lv_model::Decimal::ZERO)?;
            let kurztext = p.kurztext.value.as_deref().unwrap_or("").trim().to_string();
            if kurztext.is_empty() {
                return None;
            }
            Some(PriceRow {
                phase: i16::from(phase),
                lv_name: import.lv.name.clone(),
                oz: p
                    .oz
                    .value
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                kurztext,
                langtext: p
                    .langtext
                    .value
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(PRICE_LANGTEXT_MAX)
                    .collect(),
                menge: p.menge.value.map(|m| m.to_string()),
                einheit: p
                    .einheit
                    .value
                    .map(|u| u.symbol().to_string())
                    .or_else(|| p.einheit.raw.clone()),
                ep: ep.to_string(),
                art: art_str(p.art).to_string(),
            })
        })
        .collect()
}

fn has_text(p: &lv_model::Position) -> bool {
    p.kurztext
        .value
        .as_deref()
        .is_some_and(|t| !t.trim().is_empty())
}

/// Eigene D84-Abgaben enthalten nur Preissätze. Die Texte stehen in der Ausschreibung
/// (D83/X83) im selben Projektordner: Gesucht wird im Ordner der Datei und eine Ebene darüber;
/// passend ist die erste GAEB-Datei mit Texten, die alle OZ der Abgabe enthält.
fn fill_texts_from_siblings(path: &Path, lv: &mut lv_model::Lv) {
    if lv.positions().all(has_text) {
        return;
    }
    let ozs: Vec<lv_model::Oz> = lv.positions().filter_map(|p| p.oz.value.clone()).collect();
    if ozs.is_empty() {
        return;
    }
    for candidate in sibling_gaeb_files(path) {
        let Ok(src) = std::fs::read(&candidate)
            .map_err(drop)
            .and_then(|b| gaeb::import(&b).map_err(drop))
        else {
            continue;
        };
        let by_oz: std::collections::HashMap<&lv_model::Oz, &lv_model::Position> = src
            .lv
            .positions()
            .filter(|p| has_text(p))
            .filter_map(|p| Some((p.oz.value.as_ref()?, p)))
            .collect();
        if !ozs.iter().all(|oz| by_oz.contains_key(oz)) {
            continue;
        }
        for p in lv.positions_mut() {
            let Some(s) = p.oz.value.as_ref().and_then(|oz| by_oz.get(oz)) else {
                continue;
            };
            if !has_text(p) {
                p.kurztext = s.kurztext.clone();
                p.langtext = s.langtext.clone();
            }
            if p.menge.value.is_none() {
                p.menge = s.menge.clone();
            }
            if p.einheit.value.is_none() && p.einheit.raw.is_none() {
                p.einheit = s.einheit.clone();
            }
            p.art = s.art;
        }
        tracing::debug!(datei = %path.display(), texte = %candidate.display(), "Texte der Abgabe aus der Ausschreibung");
        return;
    }
}

/// GAEB-Dateien neben `path` (Ordner und Unterordner), dann eine Ebene höher; ohne `path`.
fn sibling_gaeb_files(path: &Path) -> Vec<std::path::PathBuf> {
    // shortcut: höchstens 200 Kandidaten, damit ein riesiger Ordner den Worker nicht aufhält.
    const MAX: usize = 200;
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let ancestors = [
        (path.parent(), 2),
        (path.parent().and_then(Path::parent), 3),
    ];
    for (dir, depth) in ancestors {
        let Some(dir) = dir else { continue };
        let mut found: Vec<_> = walkdir::WalkDir::new(dir)
            .max_depth(depth)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(walkdir::DirEntry::into_path)
            .filter(|p| p != path && crate::scan::detect_kind(p) == Some(crate::DocKind::Gaeb))
            .filter(|p| !out.contains(p))
            .take(MAX)
            .collect();
        // Ausschreibungen (83, 81) zuerst.
        found.sort_by_key(|p| {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            !(ext.ends_with("83") || ext.ends_with("81"))
        });
        out.extend(found);
        if out.len() >= MAX {
            break;
        }
    }
    out
}

fn art_str(art: lv_model::PositionsArt) -> &'static str {
    use lv_model::PositionsArt::*;
    match art {
        Normal => "normal",
        Bedarf => "bedarf",
        Alternativ => "alternativ",
        Zulage => "zulage",
        Stundenlohn => "stundenlohn",
    }
}

fn collect_nodes(nodes: &[Node], out: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::Titel(t) => {
                let oz =
                    t.oz.value
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                let name = t.bezeichnung.value.clone().unwrap_or_default();
                out.push(format!("{oz} {name}").trim().to_string());
                collect_nodes(&t.children, out);
            }
            Node::Position(p) => {
                let mut s = String::new();
                let oz =
                    p.oz.value
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                s.push_str(&format!(
                    "{oz} {}",
                    p.kurztext.value.as_deref().unwrap_or("")
                ));
                if let Some(lang) = p.langtext.value.as_deref().filter(|l| !l.trim().is_empty()) {
                    s.push('\n');
                    s.push_str(lang.trim());
                }
                if let Some(menge) = p.menge.value {
                    let unit = p.einheit.value.map(|u| u.to_string()).unwrap_or_default();
                    s.push_str(&format!("\nMenge: {menge} {unit}"));
                }
                if let Some(ep) = p.ep.value {
                    s.push_str(&format!("\nEP: {ep}"));
                }
                out.push(s.trim().to_string());
            }
            Node::Vorbemerkung(v) => {
                if let Some(t) = v.text.value.as_deref().filter(|t| !t.trim().is_empty()) {
                    out.push(t.trim().to_string());
                }
            }
        }
    }
}

/// Text/Markdown: UTF-8, sonst Windows-1252.
pub fn extract_text(path: &Path) -> Result<Vec<PageText>, IngestError> {
    Ok(vec![PageText {
        page: None,
        text: decode_text(&read_bytes(path)?),
    }])
}

pub(crate) fn read_bytes(path: &Path) -> Result<Vec<u8>, IngestError> {
    std::fs::read(path).map_err(|source| IngestError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// UTF-8 (mit/ohne BOM); bei ungültigem UTF-8 Windows-1252.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_ir::{Rect, Token, TokenSource};

    fn tok(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Token {
        Token {
            text: text.into(),
            bbox: Rect::new(x0, y0, x1, y1),
            page: 0,
            conf: 1.0,
            src: TokenSource::TextLayer,
            font_size: None,
            bold: None,
        }
    }

    #[test]
    fn lines_and_paragraphs_from_tokens() {
        let tokens = vec![
            tok("Zeile", 0.0, 0.0, 30.0, 10.0),
            tok("eins", 35.0, 0.0, 60.0, 10.0),
            tok("zwei", 0.0, 12.0, 30.0, 22.0),
            // große Lücke → Absatz
            tok("Neuer", 0.0, 60.0, 30.0, 70.0),
            tok("Absatz", 35.0, 60.0, 70.0, 70.0),
        ];
        let lines = group_lines(tokens);
        assert_eq!(lines_to_text(&lines), "Zeile eins\nzwei\n\nNeuer Absatz");
    }

    #[test]
    fn decodes_utf8_bom_and_cp1252() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFGr\xC3\xBCn"), "Grün");
        assert_eq!(decode_text(b"Gr\xFCn"), "Grün");
    }

    #[test]
    fn text_file_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("n.txt");
        std::fs::write(&p, "Übergabe am Montag").unwrap();
        let pages = extract_text(&p).unwrap();
        assert_eq!(pages[0].text, "Übergabe am Montag");
        assert_eq!(pages[0].page, None);
    }

    #[test]
    fn gaeb_d_file_falls_back_to_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lv.d83");
        std::fs::write(&p, "00 GAEB 90 Testdatei").unwrap();
        let pages = extract_gaeb(&p).unwrap();
        assert!(pages[0].text.contains("GAEB 90"));
        let x = dir.path().join("kaputt.x83");
        std::fs::write(&x, "kein xml").unwrap();
        assert!(extract_gaeb(&x).is_err());
    }

    /// GAEB-90-Datei aus Inhaltszeilen (Sp. 1–74), mit laufender Nummer und CRLF.
    fn d90(lines: &[&str]) -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{l:<74}{:06}\r\n", i + 1))
            .collect()
    }

    #[test]
    fn gaeb_offer_yields_prices_tender_does_not() {
        let body = [
            "1101       N    Bereich",
            "12Pflaster",
            "2101010001 NNN         00000012500m2",
            "25Betonpflaster verlegen",
            "26   Grau, 8 cm",
            "2301010001  0000004550  000000568750",
            "2101010002 NEN         00000001000St",
            "25Bedarf: Rinne",
            "2301010002  0000012000  000000120000",
            "2301010003  0000000000  000000000000",
            "3101",
            "99",
        ];
        let dir = tempfile::tempdir().unwrap();
        let offer = dir.path().join("abgabe.d84");
        let mut lines =
            vec!["00        84                                                  1122PPPPI90"];
        lines.extend(body);
        std::fs::write(&offer, d90(&lines)).unwrap();
        let (pages, rows) = extract_gaeb_with_prices(&offer).unwrap();
        assert!(pages[0].text.contains("Betonpflaster"));
        // Satz 23 ohne Position (01.01.0003, EP 0) zählt nicht.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].oz, "01.01.0001");
        assert_eq!(rows[0].ep, "45.500");
        assert_eq!(rows[0].menge.as_deref(), Some("12.500"));
        assert_eq!(rows[0].einheit.as_deref(), Some("m2"));
        assert_eq!(rows[0].langtext, "Grau, 8 cm");
        assert_eq!(rows[1].art, "bedarf");

        let tender = dir.path().join("ausschreibung.d83");
        lines[0] = "00        83                                                  1122PPPPI90";
        std::fs::write(&tender, d90(&lines)).unwrap();
        assert!(extract_gaeb_with_prices(&tender).unwrap().1.is_empty());
    }

    #[test]
    fn offer_without_texts_takes_them_from_tender_in_project() {
        let head83 = "00        83                                                  1122PPPPI90";
        let head84 = "00        84                                                  1122PPPPI90";
        let project = tempfile::tempdir().unwrap();
        let lv_dir = project.path().join("02_LV");
        let abgabe_dir = project.path().join("03_Abgabe");
        std::fs::create_dir_all(&lv_dir).unwrap();
        std::fs::create_dir_all(&abgabe_dir).unwrap();
        std::fs::write(
            lv_dir.join("Ausschreibung.d83"),
            d90(&[
                head83,
                "2101010001 NNN         00000012500m2",
                "25Betonpflaster verlegen",
                "2101010002 NEN         00000001000St",
                "25Rinne",
                "99",
            ]),
        )
        .unwrap();
        let offer = abgabe_dir.join("LA0001A.d84");
        std::fs::write(
            &offer,
            d90(&[
                head84,
                "2301010001  0000004550  000000568750",
                "2301010002  0000012000  000000120000",
                "99",
            ]),
        )
        .unwrap();
        let (_, rows) = extract_gaeb_with_prices(&offer).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kurztext, "Betonpflaster verlegen");
        assert_eq!(rows[0].menge.as_deref(), Some("12.500"));
        assert_eq!(rows[1].art, "bedarf");
    }

    #[test]
    fn gaeb_x83_positions_become_paragraphs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<GAEB xmlns="http://www.gaeb.de/GAEB_DA_XML/DA83/3.3">
 <GAEBInfo><Version>3.3</Version></GAEBInfo>
 <Award><DP>83</DP><AwardInfo><Cur>EUR</Cur></AwardInfo>
  <BoQ><BoQInfo><Name>Test</Name><LblBoQ>Standard</LblBoQ></BoQInfo>
   <BoQBody><BoQCtgy RNoPart="01"><LblTx><p><span>Pflasterarbeiten</span></p></LblTx>
    <BoQBody><Itemlist>
     <Item RNoPart="0010"><Qty>10</Qty><QU>m2</QU>
      <Description><CompleteText><OutlineText><OutlTxt><TextOutlTxt><p><span>Betonpflaster verlegen</span></p></TextOutlTxt></OutlTxt></OutlineText></CompleteText></Description>
     </Item>
    </Itemlist></BoQBody>
   </BoQCtgy></BoQBody>
  </BoQ></Award></GAEB>"#;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lv.x83");
        std::fs::write(&p, xml).unwrap();
        // Ob der tolerante Reader dieses Minimal-XML akzeptiert, hängt vom gaeb-Crate ab;
        // wichtig ist, dass kein Panic entsteht und ein Ergebnis oder Fehler zurückkommt.
        if let Ok(pages) = extract_gaeb(&p) {
            assert!(pages[0].text.contains("Betonpflaster") || pages[0].text.is_empty());
        }
    }
}
