//! PDF-Zugriff über PDFium: Textebene mit Bounding-Boxes, Seiten-Triage und Rendering für OCR.
//!
//! PDFium wird zur Laufzeit dynamisch geladen (Binaries: github.com/bblanchon/pdfium-binaries).
//! PDFium ist nicht thread-safe; pro Prozess gibt es genau eine [`PdfEngine`], parallelisiert
//! wird über mehrere Worker-Prozesse.

mod engine;
mod triage;
mod words;

pub use engine::{PdfDoc, PdfEngine, PdfError};
pub use triage::{OcrReason, PageDecision, TextLayerStats};
pub use words::{CharBox, words_from_chars};
