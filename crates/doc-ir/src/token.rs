use serde::{Deserialize, Serialize};

use crate::Rect;

/// Woher ein Token stammt. Bestimmt u. a., wie stark der Konfidenz zu trauen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenSource {
    /// Eingebettete Textebene eines digitalen PDFs.
    TextLayer,
    /// Klassisches OCR (Detektion + Erkennung).
    Ocr,
    /// Vision-Language-Modell, z. B. für Handschrift oder schwierige Tabellen.
    Vlm,
    /// Zelle aus der Tabellenstrukturerkennung.
    TableCell {
        row: u32,
        col: u32,
    },
    Gaeb,
    Excel {
        row: u32,
        col: u32,
    },
    /// Vom Nutzer im Review korrigiert.
    Manual,
}

/// Kleinste Texteinheit (in der Regel ein Wort) mit Position und Konfidenz.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub text: String,
    pub bbox: Rect,
    /// Seite, 0-basiert.
    pub page: u32,
    /// 0..=1; Textebene und strukturierte Quellen liefern 1.0.
    pub conf: f32,
    pub src: TokenSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
}

/// Wie der Inhalt einer Seite gewonnen wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PageSource {
    TextLayer,
    Ocr { dpi: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageInfo {
    /// 0-basiert.
    pub index: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub source: PageSource,
}

/// Eine Seite mit allen erkannten Tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub info: PageInfo,
    pub tokens: Vec<Token>,
}

impl Page {
    /// Mittlere Konfidenz aller Tokens, `None` bei leerer Seite.
    pub fn mean_conf(&self) -> Option<f32> {
        if self.tokens.is_empty() {
            return None;
        }
        Some(self.tokens.iter().map(|t| t.conf).sum::<f32>() / self.tokens.len() as f32)
    }
}
