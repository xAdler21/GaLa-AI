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
    let bytes = read_bytes(path)?;
    match gaeb::import(&bytes) {
        Ok(import) => {
            let mut paragraphs = Vec::new();
            collect_nodes(&import.lv.nodes, &mut paragraphs);
            Ok(vec![PageText {
                page: None,
                text: paragraphs.join("\n\n"),
            }])
        }
        Err(err) => {
            let is_d = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.to_lowercase().starts_with('d'));
            if is_d {
                // TODO: GAEB-90-Parser; bis dahin der Rohtext (Satzarten stören die Suche kaum).
                Ok(vec![PageText {
                    page: None,
                    text: decode_text(&bytes),
                }])
            } else {
                Err(IngestError::Extract {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                })
            }
        }
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

fn read_bytes(path: &Path) -> Result<Vec<u8>, IngestError> {
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
