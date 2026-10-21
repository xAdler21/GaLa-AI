//! Zwischenrepräsentation, auf die alle Eingangsquellen abgebildet werden.
//!
//! Egal ob ein Wert aus der PDF-Textebene, aus OCR, einem VLM, einer GAEB-Datei oder Excel
//! stammt: Er landet als [`Token`] mit Bounding-Box, Konfidenz und Herkunft hier. Darauf baut
//! die LV-Strukturerkennung auf, und das UI kann zu jedem erkannten Wert die Fundstelle zeigen.
//!
//! Koordinaten sind PDF-Punkte (1/72 Zoll) mit Ursprung **oben links**.

mod geom;
mod line;
mod token;

pub use geom::{Rect, SourceRef};
pub use line::{Line, group_lines};
pub use token::{Page, PageInfo, PageSource, Token, TokenSource};
