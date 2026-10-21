//! Zahlen aus LVs: deutsches und englisches Format, Währungszeichen, OCR-Verwechsler.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Zahlenformat eines Dokuments. Wird pro Dokument per Mehrheitsentscheid bestimmt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberLocale {
    /// `1.234,56`
    #[default]
    German,
    /// `1,234.56`
    English,
}

impl NumberLocale {
    fn separators(self) -> (char, char) {
        match self {
            NumberLocale::German => ('.', ','),
            NumberLocale::English => (',', '.'),
        }
    }
}

/// Parst Zahlen wie `1.234,56`, `1 234,5`, `-12,00 €`, `123,45-` oder `1250`.
///
/// Tausendergruppen werden streng geprüft (genau drei Ziffern). Ein OCR-Fehler wie `1.23,45`
/// ergibt daher `None` statt eines falschen Werts.
pub fn parse_decimal(raw: &str, locale: NumberLocale) -> Option<Decimal> {
    let mut s = raw.trim();
    for suffix in ["€", "EUR", "Eur", "eur"] {
        s = s.strip_suffix(suffix).unwrap_or(s).trim_end();
    }

    let mut negative = false;
    if let Some(rest) = s.strip_prefix(['-', '−']) {
        negative = true;
        s = rest;
    } else if let Some(rest) = s.strip_suffix('-') {
        // Manche AVA-Programme drucken Gutschriften als "123,45-".
        negative = true;
        s = rest;
    }

    let compact: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{202f}' | '\''))
        .collect();
    if compact.is_empty() {
        return None;
    }

    let (thousands, decimal) = locale.separators();
    let (int_part, frac_part) = match compact.rsplit_once(decimal) {
        Some((i, f)) => (i, Some(f)),
        // Ohne Dezimaltrennzeichen: "1.250" ist eine Tausendergruppe, "12.5" (falsches Format
        // im Dokument) ein Dezimalbruch.
        None => match compact.rsplit_once(thousands) {
            Some((i, f)) if f.len() != 3 && !i.contains(thousands) => (i, Some(f)),
            _ => (compact.as_str(), None),
        },
    };

    let mut groups = int_part.split(thousands);
    let first = groups.next()?;
    let rest: Vec<&str> = groups.collect();
    let int_ok = !first.is_empty()
        && first.chars().all(|c| c.is_ascii_digit())
        && (rest.is_empty() || first.len() <= 3)
        && rest
            .iter()
            .all(|g| g.len() == 3 && g.chars().all(|c| c.is_ascii_digit()));
    if !int_ok {
        return None;
    }
    if let Some(f) = frac_part
        && (f.is_empty() || !f.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }

    let mut normalized = String::with_capacity(compact.len() + 1);
    if negative {
        normalized.push('-');
    }
    normalized.push_str(first);
    for g in rest {
        normalized.push_str(g);
    }
    if let Some(f) = frac_part {
        normalized.push('.');
        normalized.push_str(f);
    }
    Decimal::from_str(&normalized).ok()
}

/// Bestimmt das Zahlenformat anhand eindeutiger Stichproben eines Dokuments.
/// Bei Gleichstand oder ohne Hinweise: deutsch.
pub fn detect_locale<'a>(samples: impl IntoIterator<Item = &'a str>) -> NumberLocale {
    let (mut german, mut english) = (0u32, 0u32);
    for s in samples {
        let last_comma = s.rfind(',');
        let last_dot = s.rfind('.');
        match (last_comma, last_dot) {
            (Some(c), Some(d)) if c > d => german += 1,
            (Some(_), Some(_)) => english += 1,
            (Some(c), None) if s.len() - c - 1 != 3 => german += 1,
            (None, Some(d)) if s.len() - d - 1 == 2 => english += 1,
            _ => {}
        }
    }
    if english > german {
        NumberLocale::English
    } else {
        NumberLocale::German
    }
}

/// Korrigiert typische OCR-Verwechsler (O→0, l→1, S→5 …) – nur für Tokens aus Zahlenspalten
/// aufrufen, sonst werden echte Buchstaben verfälscht. Tokens, die überwiegend aus Buchstaben
/// bestehen, bleiben unverändert.
pub fn fix_ocr_digits(raw: &str) -> String {
    let numeric = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-'))
        .count();
    if numeric * 2 < raw.chars().count() {
        return raw.to_string();
    }
    raw.chars()
        .map(|c| match c {
            'O' | 'o' | 'Q' | 'D' => '0',
            'I' | 'l' | 'i' | '|' => '1',
            'Z' | 'z' => '2',
            'S' | 's' => '5',
            'G' => '6',
            'B' => '8',
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn de(s: &str) -> Option<Decimal> {
        parse_decimal(s, NumberLocale::German)
    }

    #[test]
    fn parses_german_numbers() {
        assert_eq!(de("1.234,56"), Some(dec!(1234.56)));
        assert_eq!(de("1 234,5"), Some(dec!(1234.5)));
        assert_eq!(de("1250"), Some(dec!(1250)));
        assert_eq!(de("1.250"), Some(dec!(1250)));
        assert_eq!(de("0,75"), Some(dec!(0.75)));
        assert_eq!(de("12,00 €"), Some(dec!(12.00)));
        assert_eq!(de("1.000.000,00 EUR"), Some(dec!(1000000.00)));
        assert_eq!(de("-3,50"), Some(dec!(-3.50)));
        assert_eq!(de("123,45-"), Some(dec!(-123.45)));
        // Falsches Format im deutschen Dokument: Punkt als Dezimaltrenner.
        assert_eq!(de("12.5"), Some(dec!(12.5)));
    }

    #[test]
    fn rejects_broken_numbers() {
        assert_eq!(de("1.23,45"), None);
        assert_eq!(de("12a,5"), None);
        assert_eq!(de("1234.567,8"), None);
        assert_eq!(de(""), None);
        assert_eq!(de("€"), None);
        assert_eq!(de("12,"), None);
    }

    #[test]
    fn parses_english_numbers() {
        let en = |s| parse_decimal(s, NumberLocale::English);
        assert_eq!(en("1,234.56"), Some(dec!(1234.56)));
        assert_eq!(en("1,250"), Some(dec!(1250)));
    }

    #[test]
    fn detects_locale_by_majority() {
        assert_eq!(
            detect_locale(["1.234,56", "12,50", "1250"]),
            NumberLocale::German
        );
        assert_eq!(detect_locale(["1,234.56", "12.50"]), NumberLocale::English);
        assert_eq!(detect_locale(["1250", "1.250"]), NumberLocale::German);
    }

    #[test]
    fn fixes_ocr_confusions_only_in_numeric_tokens() {
        assert_eq!(fix_ocr_digits("1.2S0,O0"), "1.250,00");
        assert_eq!(fix_ocr_digits("l5,5O"), "15,50");
        assert_eq!(fix_ocr_digits("Stck"), "Stck");
    }
}
