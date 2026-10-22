//! GAEB DA XML 3.x: Leistungsverzeichnisse lesen (X81–X86) und die Angebotsabgabe X84 schreiben.
//!
//! Der Reader ist bewusst tolerant (Namespaces und Versionen 3.1–3.3 werden ignoriert, unbekannte
//! Elemente übersprungen), weil echte Dateien aus AVA-Programmen selten schemakonform sind.
//! Der X84-Writer verändert die Original-X83 nur minimal: Er setzt Einheits- und Gesamtpreise
//! ein und stellt Phase und Namespace um. IDs, Texte und Struktur bleiben unangetastet, damit
//! Vergabeplattformen das Angebot der Ausschreibung zuordnen können.

mod decode;
mod phase;
mod read;
mod text;
mod x84;

pub use decode::decode;
pub use phase::Phase;
pub use read::GaebImport;
pub use x84::{X84Export, write_x84};

#[derive(Debug, thiserror::Error)]
pub enum GaebError {
    #[error("Datei ist kein gültiges XML")]
    Parse(#[from] roxmltree::Error),
    #[error("keine GAEB-DA-XML-Datei (Wurzelelement {0:?})")]
    NotGaeb(String),
    #[error("Pflichtelement fehlt: {0}")]
    Missing(&'static str),
    #[error("Zeichenkodierung {0:?} wird nicht unterstützt")]
    Encoding(String),
    #[error("X84 konnte nicht geschrieben werden: {0}")]
    Write(String),
}

/// Liest eine GAEB-Datei aus Rohbytes (Kodierung laut BOM bzw. XML-Deklaration).
pub fn import(bytes: &[u8]) -> Result<GaebImport, GaebError> {
    read::read(&decode(bytes)?).map(|(import, _)| import)
}

/// Liest GAEB DA XML aus einem bereits dekodierten String.
pub fn read_xml(xml: &str) -> Result<GaebImport, GaebError> {
    read::read(xml).map(|(import, _)| import)
}
