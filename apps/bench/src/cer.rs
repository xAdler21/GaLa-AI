//! Zeichenfehlerrate (CER) und Lesereihenfolge für den Textvergleich.

use doc_ir::{Line, Token, group_lines};

/// Leerraum auf einzelne Leerzeichen reduzieren; Zeilenumbrüche zählen nicht als Fehler.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text in Lesereihenfolge (Zeilen von oben nach unten, Tokens von links nach rechts).
/// Textebene und OCR laufen durch dieselbe Zeilenbildung und sind dadurch vergleichbar.
pub fn reading_order_text(tokens: Vec<Token>) -> String {
    let lines: Vec<String> = group_lines(tokens).iter().map(Line::text).collect();
    normalize(&lines.join("\n"))
}

/// Levenshtein-Distanz auf Zeichenebene geteilt durch die Länge der Referenz.
pub fn cer(reference: &str, hypothesis: &str) -> f32 {
    let r: Vec<char> = reference.chars().collect();
    let h: Vec<char> = hypothesis.chars().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0; h.len() + 1];
    for (i, rc) in r.iter().enumerate() {
        cur[0] = i + 1;
        for (j, hc) in h.iter().enumerate() {
            let substitute = prev[j] + usize::from(rc != hc);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[h.len()] as f32 / r.len() as f32
}

/// Perzentil (0..=1) per nächstem Rang; `None` bei leerer Eingabe.
pub fn percentile(values: &[f32], p: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let rank = ((sorted.len() - 1) as f32 * p).round() as usize;
    Some(sorted[rank])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cer_counts_edits_relative_to_reference() {
        assert_eq!(cer("Pflaster", "Pflaster"), 0.0);
        assert_eq!(cer("1.250,00", "1.25O,00"), 1.0 / 8.0);
        assert_eq!(cer("Sand", "Sandbett"), 1.0);
        assert_eq!(cer("", ""), 0.0);
        assert_eq!(cer("", "x"), 1.0);
        assert_eq!(cer("Kies", ""), 1.0);
    }

    #[test]
    fn umlauts_are_single_characters() {
        assert_eq!(cer("Grünfläche", "Grunflache"), 0.2);
    }

    #[test]
    fn normalize_ignores_layout_whitespace() {
        assert_eq!(
            normalize("  01.02.0030 \n Pflaster\t grau "),
            "01.02.0030 Pflaster grau"
        );
    }

    #[test]
    fn percentile_by_nearest_rank() {
        let v = [0.05, 0.01, 0.03, 0.02, 0.04];
        assert_eq!(percentile(&v, 0.5), Some(0.03));
        assert_eq!(percentile(&v, 1.0), Some(0.05));
        assert_eq!(percentile(&[], 0.5), None);
    }
}
