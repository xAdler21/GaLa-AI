//! OCR-Abstraktion. Der Rest des Systems kennt nur [`OcrEngine`]; die Implementierung
//! (heute PaddleOCR über oar-ocr, bei Bedarf ein Python-Sidecar) ist austauschbar.

use doc_ir::{Page, Rect, Token, TokenSource};
use image::RgbImage;

#[cfg(feature = "paddle")]
mod paddle;
#[cfg(feature = "paddle")]
pub use paddle::{Device, PaddleOcr, Preset};

/// Eine gerenderte Seite als OCR-Eingabe.
pub struct PageImage {
    /// 0-basiert.
    pub page: u32,
    /// Auflösung, mit der gerendert wurde; rechnet Pixel in PDF-Punkte um.
    pub dpi: u32,
    pub image: RgbImage,
}

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("OCR-Engine konnte nicht initialisiert werden: {0}")]
    Init(String),
    #[error("OCR fehlgeschlagen: {0}")]
    Inference(String),
}

pub trait OcrEngine {
    fn name(&self) -> &str;

    /// Erkennt Text auf gerenderten Seiten. Mehrere Seiten pro Aufruf lasten die GPU besser aus.
    fn recognize(&self, pages: Vec<PageImage>) -> Result<Vec<Page>, OcrError>;
}

/// Zerlegt eine erkannte Textzeile in Wort-Tokens (Koordinaten in Punkten).
///
/// Mit Zeichen-Boxen (eine pro Zeichen, inkl. Leerzeichen) entstehen echte Wort-Boxen; das ist
/// wichtig, weil eine OCR-Zeile mehrere LV-Spalten überspannen kann. Ohne passende Zeichen-Boxen
/// wird die ganze Zeile ein Token.
pub fn line_to_tokens(
    text: &str,
    line_bbox: Rect,
    char_boxes: Option<&[Rect]>,
    conf: f32,
    page: u32,
) -> Vec<Token> {
    let token = |text: String, bbox: Rect| Token {
        text,
        bbox,
        page,
        conf,
        src: TokenSource::Ocr,
        font_size: None,
        bold: None,
    };

    let chars: Vec<char> = text.chars().collect();
    let Some(boxes) = char_boxes.filter(|b| b.len() == chars.len()) else {
        let trimmed = text.trim();
        return if trimmed.is_empty() {
            Vec::new()
        } else {
            vec![token(trimmed.to_string(), line_bbox)]
        };
    };

    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut word_box: Option<Rect> = None;
    for (c, b) in chars.iter().zip(boxes) {
        if c.is_whitespace() {
            if let Some(bbox) = word_box.take() {
                tokens.push(token(std::mem::take(&mut word), bbox));
            }
            continue;
        }
        word.push(*c);
        word_box = Some(word_box.map_or(*b, |acc| acc.union(b)));
    }
    if let Some(bbox) = word_box {
        tokens.push(token(word, bbox));
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_line_into_words_with_char_boxes() {
        let text = "0030 Pflaster";
        let boxes: Vec<Rect> = (0..text.chars().count())
            .map(|i| Rect::new(i as f32 * 10.0, 0.0, i as f32 * 10.0 + 8.0, 12.0))
            .collect();
        let tokens = line_to_tokens(
            text,
            Rect::new(0.0, 0.0, 128.0, 12.0),
            Some(&boxes),
            0.97,
            2,
        );
        let words: Vec<(&str, f32, f32)> = tokens
            .iter()
            .map(|t| (t.text.as_str(), t.bbox.x0, t.bbox.x1))
            .collect();
        assert_eq!(words, [("0030", 0.0, 38.0), ("Pflaster", 50.0, 128.0)]);
        assert!(
            tokens
                .iter()
                .all(|t| t.conf == 0.97 && t.page == 2 && t.src == TokenSource::Ocr)
        );
    }

    #[test]
    fn falls_back_to_line_token_without_matching_boxes() {
        let line = Rect::new(0.0, 0.0, 100.0, 12.0);
        let tokens = line_to_tokens(" Pflaster grau ", line, Some(&[line]), 0.9, 0);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "Pflaster grau");
        assert!(line_to_tokens("   ", line, None, 0.9, 0).is_empty());
    }
}
