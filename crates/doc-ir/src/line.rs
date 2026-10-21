use serde::{Deserialize, Serialize};

use crate::{Rect, Token};

/// Tokens einer Textzeile, von links nach rechts sortiert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub page: u32,
    pub bbox: Rect,
    pub tokens: Vec<Token>,
}

impl Line {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&t.text);
        }
        out
    }

    /// Schwächstes Glied der Zeile; leere Zeilen gelten als sicher.
    pub fn min_conf(&self) -> f32 {
        self.tokens.iter().map(|t| t.conf).fold(1.0, f32::min)
    }
}

/// Mindestüberlappung (relativ zur kleineren Höhe), ab der ein Token zu einer Zeile gehört.
const MIN_LINE_OVERLAP: f32 = 0.5;

struct LineBuilder {
    page: u32,
    tokens: Vec<Token>,
    sum_y0: f32,
    sum_y1: f32,
}

impl LineBuilder {
    /// Mittleres y-Band der bisherigen Tokens. Robuster als die Gesamt-Bbox, die durch einen
    /// einzelnen hohen Token (große Schrift, OCR-Ausreißer) zwei Zeilen verschmelzen würde.
    fn band(&self) -> Rect {
        let n = self.tokens.len() as f32;
        Rect::new(0.0, self.sum_y0 / n, 1.0, self.sum_y1 / n)
    }

    fn push(&mut self, token: Token) {
        self.sum_y0 += token.bbox.y0;
        self.sum_y1 += token.bbox.y1;
        self.tokens.push(token);
    }
}

/// Gruppiert Tokens anhand vertikaler Überlappung zu Zeilen.
///
/// Ergebnis ist nach Seite und y-Position sortiert, Tokens innerhalb einer Zeile nach x.
/// Die Spalten eines LVs (OZ, Text, Menge, Einheit, EP, GP) landen dabei bewusst in derselben
/// Zeile; die Spaltenzuordnung passiert später im Parser.
pub fn group_lines(tokens: impl IntoIterator<Item = Token>) -> Vec<Line> {
    let mut tokens: Vec<Token> = tokens.into_iter().collect();
    tokens.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then(a.bbox.center_y().total_cmp(&b.bbox.center_y()))
            .then(a.bbox.x0.total_cmp(&b.bbox.x0))
    });

    let mut builders: Vec<LineBuilder> = Vec::new();
    let mut page_start = 0;
    for token in tokens {
        if builders.last().is_some_and(|b| b.page != token.page) {
            page_start = builders.len();
        }

        let best = builders[page_start..]
            .iter()
            .enumerate()
            .map(|(i, b)| (page_start + i, b.band().vertical_overlap_ratio(&token.bbox)))
            .filter(|&(_, ratio)| ratio >= MIN_LINE_OVERLAP)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);

        match best {
            Some(i) => builders[i].push(token),
            None => {
                let mut b = LineBuilder {
                    page: token.page,
                    tokens: Vec::new(),
                    sum_y0: 0.0,
                    sum_y1: 0.0,
                };
                b.push(token);
                builders.push(b);
            }
        }
    }

    let mut lines: Vec<Line> = builders
        .into_iter()
        .map(|mut b| {
            b.tokens.sort_by(|x, y| x.bbox.x0.total_cmp(&y.bbox.x0));
            let bbox = b
                .tokens
                .iter()
                .skip(1)
                .fold(b.tokens[0].bbox, |acc, t| acc.union(&t.bbox));
            Line {
                page: b.page,
                bbox,
                tokens: b.tokens,
            }
        })
        .collect();
    lines.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then(a.bbox.center_y().total_cmp(&b.bbox.center_y()))
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenSource;

    fn tok(text: &str, page: u32, x0: f32, y0: f32, x1: f32, y1: f32) -> Token {
        Token {
            text: text.into(),
            bbox: Rect::new(x0, y0, x1, y1),
            page,
            conf: 1.0,
            src: TokenSource::TextLayer,
            font_size: None,
            bold: None,
        }
    }

    #[test]
    fn groups_lv_row_into_one_line_in_reading_order() {
        // Eine LV-Zeile, leicht schief gescannt, in zufälliger Reihenfolge geliefert.
        let tokens = vec![
            tok("m2", 0, 400.0, 101.0, 415.0, 111.0),
            tok("01.02.0030", 0, 40.0, 100.0, 90.0, 110.0),
            tok("Betonpflaster", 0, 100.0, 100.5, 170.0, 110.5),
            tok("1.250,00", 0, 340.0, 101.0, 390.0, 111.0),
            tok("Langtextzeile", 0, 100.0, 114.0, 170.0, 124.0),
        ];
        let lines = group_lines(tokens);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "01.02.0030 Betonpflaster 1.250,00 m2");
        assert_eq!(lines[1].text(), "Langtextzeile");
    }

    #[test]
    fn never_merges_across_pages() {
        let tokens = vec![
            tok("Seite2", 1, 10.0, 10.0, 50.0, 20.0),
            tok("Seite1", 0, 10.0, 10.0, 50.0, 20.0),
        ];
        let lines = group_lines(tokens);
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].page, lines[0].text().as_str()), (0, "Seite1"));
        assert_eq!((lines[1].page, lines[1].text().as_str()), (1, "Seite2"));
    }

    #[test]
    fn tall_token_does_not_swallow_neighbouring_lines() {
        let tokens = vec![
            tok("Zeile1", 0, 10.0, 0.0, 50.0, 10.0),
            tok("GROSS", 0, 60.0, 0.0, 120.0, 24.0),
            tok("Zeile2", 0, 10.0, 14.0, 50.0, 24.0),
        ];
        let lines = group_lines(tokens);
        let texts: Vec<String> = lines.iter().map(Line::text).collect();
        assert!(texts.contains(&"Zeile2".to_string()), "{texts:?}");
    }
}
