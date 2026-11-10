//! Indizierung des Netzlaufwerks: scannen, Text gewinnen, chunken, Aufgaben in die Datenbank.
//!
//! Ablauf (siehe `index_task`):
//! 1. [`store::sync`] gleicht das Laufwerk mit `document` ab und legt `extract`-Tasks an.
//! 2. `extract`: Textebene lesen ([`extract`]), Chunks speichern ([`chunk`]); Seiten ohne brauchbare
//!    Textebene werden `ocr_page`-Tasks, sonst folgt `embed`.
//! 3. `ocr_page` (GPU-Worker): Seite rendern, OCR, Chunks speichern, `embed`.
//! 4. `embed`: Embeddings für Chunks ohne Vektor (Crate `retrieval`).

use std::path::PathBuf;

pub mod chunk;
pub mod extract;
pub mod scan;
pub mod store;

pub use chunk::{Chunk, ChunkConfig, PageText, chunk_pages, chunk_text};
pub use scan::{DocKind, ScannedFile, detect_kind, diff, scan_dir, sha256_file};
pub use store::{QueueConfig, SyncReport, Task, TaskKind};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("Dateizugriff auf {path} fehlgeschlagen")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("PDF-Fehler")]
    Pdf(#[from] pdf::PdfError),
    #[error("Text aus {path} nicht extrahierbar: {message}")]
    Extract { path: PathBuf, message: String },
    #[error("Datenbankfehler")]
    Db(#[from] sqlx::Error),
    #[error(
        "Scan von {0} ist leer, der Index aber nicht – Laufwerk nicht eingebunden? Abbruch ohne Löschen"
    )]
    EmptyArchive(PathBuf),
    #[error("{0}")]
    Other(String),
}
