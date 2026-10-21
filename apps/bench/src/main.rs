//! M0-Benchmark: misst an echten LVs, wie gut Textebene und OCR funktionieren, bevor die
//! Import-Pipeline darauf aufbaut.
//!
//! ```text
//! bench text <PDFs|Ordner>...                        Seiten-Triage: Textebene oder OCR?
//! bench ocr  <PDFs|Bilder|Ordner>... --preset ...    OCR-Tempo, Konfidenz, Zeichenfehlerrate
//! ```

mod cer;
mod inputs;
mod ocr_cmd;
mod text_cmd;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    version,
    about = "M0-Benchmark für das Einlesen von LVs (Textebene, OCR)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Liest die Textebene aller Seiten und entscheidet je Seite: Textebene oder OCR.
    Text(text_cmd::Args),
    /// Führt OCR aus und misst Tempo, Konfidenz und Zeichenfehlerrate.
    Ocr(ocr_cmd::Args),
}

fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn,oar_ocr=warn,oar_ocr_core=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().cmd {
        Cmd::Text(args) => text_cmd::run(args),
        Cmd::Ocr(args) => ocr_cmd::run(args),
    }
}
