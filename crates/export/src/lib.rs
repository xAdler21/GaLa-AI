//! Angebotsausgabe. GAEB X84 liegt im Crate `gaeb`; hier entstehen die Formate für Menschen.

mod xlsx;

pub use xlsx::{XlsxOptions, lv_to_xlsx};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("Excel-Datei konnte nicht erzeugt werden")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
}
