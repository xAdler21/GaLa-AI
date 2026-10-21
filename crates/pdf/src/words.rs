use doc_ir::{Rect, Token, TokenSource};

/// Ein Zeichen der Textebene mit Box (Ursprung oben links, PDF-Punkte).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharBox {
    /// Nicht abbildbare Zeichen (kaputte ToUnicode-Map) kommen als `U+FFFD` an.
    pub ch: char,
    pub bbox: Rect,
    pub font_size: f32,
    pub bold: bool,
}

/// Abstand (relativ zur Schriftgröße), ab dem zwei Zeichen zu verschiedenen Wörtern gehören.
/// Wortabstände liegen typisch bei 0,25–0,33 em, Buchstabenabstände deutlich darunter.
const WORD_GAP_EM: f32 = 0.2;

/// Fasst Zeichen in Content-Stream-Reihenfolge zu Wort-Tokens zusammen.
///
/// Ein neues Wort beginnt bei Leerraum, bei einer Lücke > [`WORD_GAP_EM`], beim Zeilenwechsel
/// oder wenn die Schreibrichtung zurückspringt (neuer Textblock).
pub fn words_from_chars(chars: impl IntoIterator<Item = CharBox>, page: u32) -> Vec<Token> {
    let mut words = Vec::new();
    let mut current: Vec<CharBox> = Vec::new();

    for c in chars {
        if c.ch.is_whitespace() || c.ch.is_control() {
            flush(&mut current, page, &mut words);
            continue;
        }
        if let Some(prev) = current.last() {
            let em = prev
                .font_size
                .max(c.font_size)
                .max(prev.bbox.height())
                .max(1.0);
            let gap = c.bbox.x0 - prev.bbox.x1;
            let same_line = prev.bbox.vertical_overlap_ratio(&c.bbox) >= 0.5;
            if !same_line || gap > WORD_GAP_EM * em || gap < -0.5 * em {
                flush(&mut current, page, &mut words);
            }
        }
        current.push(c);
    }
    flush(&mut current, page, &mut words);
    words
}

fn flush(current: &mut Vec<CharBox>, page: u32, out: &mut Vec<Token>) {
    let Some(first) = current.first() else {
        return;
    };
    let bbox = current
        .iter()
        .skip(1)
        .fold(first.bbox, |acc, c| acc.union(&c.bbox));
    let bold = current.iter().filter(|c| c.bold).count() * 2 > current.len();
    let font_size = current.iter().map(|c| c.font_size).fold(0.0, f32::max);
    out.push(Token {
        text: current.iter().map(|c| c.ch).collect(),
        bbox,
        page,
        conf: 1.0,
        src: TokenSource::TextLayer,
        font_size: (font_size > 0.0).then_some(font_size),
        bold: Some(bold),
    });
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zeichen einer Zeile mit fester Breite 5pt, Höhe 10pt; `' '` wird als Leerzeichen geliefert.
    fn line(text: &str, x: f32, y: f32, advance: f32) -> Vec<CharBox> {
        text.chars()
            .enumerate()
            .map(|(i, ch)| {
                let x0 = x + i as f32 * advance;
                CharBox {
                    ch,
                    bbox: Rect::new(x0, y, x0 + 5.0, y + 10.0),
                    font_size: 10.0,
                    bold: false,
                }
            })
            .collect()
    }

    fn texts(tokens: &[Token]) -> Vec<&str> {
        tokens.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn splits_on_whitespace() {
        let words = words_from_chars(line("Betonpflaster liefern", 0.0, 0.0, 5.0), 0);
        assert_eq!(texts(&words), ["Betonpflaster", "liefern"]);
        assert_eq!(words[0].bbox, Rect::new(0.0, 0.0, 65.0, 10.0));
    }

    #[test]
    fn splits_columns_without_space_characters() {
        // Viele AVA-PDFs positionieren Spalten per Koordinate statt mit Leerzeichen.
        let mut chars = line("0030", 40.0, 100.0, 5.0);
        chars.extend(line("Pflaster", 100.0, 100.0, 5.0));
        chars.extend(line("1.250,00", 340.0, 100.0, 5.0));
        let words = words_from_chars(chars, 0);
        assert_eq!(texts(&words), ["0030", "Pflaster", "1.250,00"]);
    }

    #[test]
    fn splits_on_line_change() {
        let mut chars = line("Ende", 0.0, 0.0, 5.0);
        chars.extend(line("Anfang", 20.0, 14.0, 5.0));
        let words = words_from_chars(chars, 3);
        assert_eq!(texts(&words), ["Ende", "Anfang"]);
        assert!(words.iter().all(|w| w.page == 3));
    }

    #[test]
    fn bold_by_majority() {
        let mut chars = line("Titel", 0.0, 0.0, 5.0);
        for c in &mut chars[..4] {
            c.bold = true;
        }
        let words = words_from_chars(chars, 0);
        assert_eq!(words[0].bold, Some(true));
    }
}
