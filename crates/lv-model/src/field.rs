use doc_ir::SourceRef;
use serde::{Deserialize, Serialize};

/// Grund, warum ein Feld vom Nutzer geprüft werden sollte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewIssue {
    /// Rohtext vorhanden, aber nicht interpretierbar (z. B. kaputte Zahl).
    Unparseable,
    /// GP ≠ Menge × EP.
    ArithmeticMismatch,
    /// Summe der Positionen ≠ ausgewiesene Titelsumme.
    SumMismatch,
    /// OZ nicht aufsteigend.
    OzOrder,
    /// OZ passt nicht zur Maske des Dokuments.
    OzMask,
    /// Einheit nicht im Wörterbuch.
    UnknownUnit,
    /// Vom LLM abgeleitet statt direkt gelesen.
    LlmInferred,
}

/// Ein erkannter Wert mit Rohtext, Konfidenz und Fundstellen im Originaldokument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field<T> {
    pub value: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// 0..=1
    pub conf: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prov: Vec<SourceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ReviewIssue>,
}

impl<T> Field<T> {
    /// Wert aus einer strukturierten Quelle (GAEB, manuelle Eingabe): volle Konfidenz.
    pub fn certain(value: T) -> Self {
        Self {
            value: Some(value),
            ..Self::absent()
        }
    }

    /// Im Dokument nicht vorhanden, z. B. der EP in einer Ausschreibung. Kein Fehler.
    pub fn absent() -> Self {
        Self {
            value: None,
            raw: None,
            conf: 1.0,
            prov: Vec::new(),
            issues: Vec::new(),
        }
    }

    /// Aus einer strukturierten Quelle (GAEB, Excel) ohne Seitenposition geparst. Ist der Rohtext
    /// nicht interpretierbar (`value == None`), wird das Feld fürs Review markiert.
    pub fn parsed(value: Option<T>, raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let issues = if value.is_none() && !raw.trim().is_empty() {
            vec![ReviewIssue::Unparseable]
        } else {
            Vec::new()
        };
        Self {
            value,
            raw: Some(raw),
            conf: 1.0,
            prov: Vec::new(),
            issues,
        }
    }

    /// Aus Textebene/OCR gelesen, mit Konfidenz und Fundstelle auf der Seite.
    pub fn recognized(
        value: Option<T>,
        raw: impl Into<String>,
        conf: f32,
        prov: SourceRef,
    ) -> Self {
        Self {
            conf,
            prov: vec![prov],
            ..Self::parsed(value, raw)
        }
    }

    pub fn flag(&mut self, issue: ReviewIssue) {
        if !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
    }

    pub fn needs_review(&self, threshold: f32) -> bool {
        !self.issues.is_empty() || self.conf < threshold
    }
}

impl<T> Default for Field<T> {
    fn default() -> Self {
        Self::absent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_ir::Rect;

    fn here() -> SourceRef {
        SourceRef {
            page: 0,
            bbox: Rect::new(0.0, 0.0, 10.0, 10.0),
        }
    }

    #[test]
    fn unparseable_raw_text_is_flagged() {
        let f: Field<u32> = Field::recognized(None, "1.2S", 0.95, here());
        assert_eq!(f.issues, [ReviewIssue::Unparseable]);
        assert!(f.needs_review(0.9));

        let empty: Field<u32> = Field::recognized(None, "  ", 0.95, here());
        assert!(!empty.needs_review(0.9));
    }

    #[test]
    fn low_confidence_needs_review() {
        let f = Field::recognized(Some(12), "12", 0.7, here());
        assert!(f.needs_review(0.9));
        assert!(!f.needs_review(0.6));
    }

    #[test]
    fn flag_is_idempotent() {
        let mut f = Field::certain(1);
        f.flag(ReviewIssue::ArithmeticMismatch);
        f.flag(ReviewIssue::ArithmeticMismatch);
        assert_eq!(f.issues.len(), 1);
    }
}
