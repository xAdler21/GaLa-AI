use doc_ir::Token;

/// Kennzahlen der Textebene einer Seite. Grundlage der Entscheidung Textebene vs. OCR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLayerStats {
    /// Nicht-Leerzeichen.
    pub chars: usize,
    /// Anteil unlesbarer Zeichen: U+FFFD, Private-Use-Area, Steuerzeichen, Mojibake (`Ã¤`).
    pub garbage_ratio: f32,
    /// Anteil plausibler Wörter unter den rein alphabetischen Wörtern ab 4 Buchstaben.
    /// Vorläufig eine Vokal-Heuristik; ein Wörterbuchabgleich folgt mit dem Golden-Corpus.
    pub plausible_word_ratio: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrReason {
    /// Keine oder fast keine Textebene (typisch für Scans).
    NoText,
    /// Textebene vorhanden, aber kaputt kodiert.
    Garbage,
    /// Zeichen lesbar, ergeben aber keine Wörter (z. B. verschobene Glyph-Zuordnung).
    Implausible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDecision {
    TextLayer,
    Ocr(OcrReason),
}

const MIN_CHARS: usize = 40;
const MAX_GARBAGE_RATIO: f32 = 0.05;
const MIN_PLAUSIBLE_WORD_RATIO: f32 = 0.6;

impl TextLayerStats {
    pub fn from_tokens(tokens: &[Token]) -> Self {
        let mut chars = 0usize;
        let mut garbage = 0usize;
        let (mut alpha_words, mut plausible_words) = (0usize, 0usize);

        for token in tokens {
            let mut prev: Option<char> = None;
            for c in token.text.chars() {
                if c.is_whitespace() {
                    continue;
                }
                chars += 1;
                let mojibake =
                    matches!(prev, Some('Ã' | 'Â')) && ('\u{80}'..='\u{bf}').contains(&c);
                if is_garbage(c) || mojibake {
                    garbage += 1;
                }
                prev = Some(c);
            }

            let letters = token.text.chars().count();
            if letters >= 4 && token.text.chars().all(char::is_alphabetic) {
                alpha_words += 1;
                if is_plausible_word(&token.text) {
                    plausible_words += 1;
                }
            }
        }

        Self {
            chars,
            garbage_ratio: ratio(garbage, chars),
            plausible_word_ratio: ratio(plausible_words, alpha_words),
        }
    }

    pub fn decision(&self) -> PageDecision {
        if self.chars < MIN_CHARS {
            PageDecision::Ocr(OcrReason::NoText)
        } else if self.garbage_ratio > MAX_GARBAGE_RATIO {
            PageDecision::Ocr(OcrReason::Garbage)
        } else if self.plausible_word_ratio < MIN_PLAUSIBLE_WORD_RATIO {
            PageDecision::Ocr(OcrReason::Implausible)
        } else {
            PageDecision::TextLayer
        }
    }
}

fn ratio(part: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        part as f32 / total as f32
    }
}

fn is_garbage(c: char) -> bool {
    c == '\u{FFFD}' || c.is_control() || ('\u{E000}'..='\u{F8FF}').contains(&c)
}

/// Deutsche Wörter haben Vokale; kaputt kodierter Text („Bxqtzl“) meist kaum.
fn is_plausible_word(word: &str) -> bool {
    let total = word.chars().count();
    let vowels = word
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| "aeiouyäöü".contains(*c))
        .count();
    vowels * 5 >= total
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_ir::{Rect, TokenSource};

    fn tokens(text: &str) -> Vec<Token> {
        text.split_whitespace()
            .map(|w| Token {
                text: w.into(),
                bbox: Rect::default(),
                page: 0,
                conf: 1.0,
                src: TokenSource::TextLayer,
                font_size: None,
                bold: None,
            })
            .collect()
    }

    const LV_TEXT: &str = "01.02.0030 Betonpflaster 20x10x8 cm grau liefern und im Sandbett \
        verlegen, Fugen mit Brechsand verfüllen und abrütteln 1.250,00 m2";

    #[test]
    fn clean_text_layer_is_used() {
        let stats = TextLayerStats::from_tokens(&tokens(LV_TEXT));
        assert_eq!(stats.garbage_ratio, 0.0);
        assert_eq!(stats.decision(), PageDecision::TextLayer);
    }

    #[test]
    fn empty_page_needs_ocr() {
        let stats = TextLayerStats::from_tokens(&tokens("Seite 1"));
        assert_eq!(stats.decision(), PageDecision::Ocr(OcrReason::NoText));
    }

    #[test]
    fn broken_encoding_needs_ocr() {
        let broken: String = LV_TEXT
            .chars()
            .map(|c| if c.is_alphabetic() { '\u{FFFD}' } else { c })
            .collect();
        let stats = TextLayerStats::from_tokens(&tokens(&broken));
        assert_eq!(stats.decision(), PageDecision::Ocr(OcrReason::Garbage));
    }

    #[test]
    fn mojibake_counts_as_garbage() {
        let text = "GrÃ¼nflÃ¤che herstellen, OberbÃ¶den lockern ".repeat(4);
        let stats = TextLayerStats::from_tokens(&tokens(&text));
        assert!(stats.garbage_ratio > MAX_GARBAGE_RATIO, "{stats:?}");
    }

    #[test]
    fn shifted_glyphs_are_implausible() {
        // Typisches Bild einer verschobenen Glyph-Zuordnung: lesbare, aber sinnlose Buchstaben.
        let text = "Bxqtzl Kmrstw Pflqrt Vbndkr Zrtpls Grmthk Nbcdfg Hjklmn Qrstvw Xyzbcd";
        let stats = TextLayerStats::from_tokens(&tokens(text));
        assert_eq!(stats.decision(), PageDecision::Ocr(OcrReason::Implausible));
    }
}
