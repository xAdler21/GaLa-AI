use std::path::PathBuf;
use std::time::Instant;

use doc_ir::group_lines;
use pdf::{OcrReason, PageDecision};
use serde::Serialize;
use tracing::{info, warn};

use crate::inputs::{collect, load_pdf_engine};

#[derive(clap::Args)]
pub struct Args {
    /// PDF-Dateien oder Ordner (rekursiv)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Verzeichnis mit der PDFium-Bibliothek
    #[arg(long, env = "PDFIUM_DIR")]
    pdfium_dir: Option<PathBuf>,
    /// Ergebnisse zusätzlich als JSON schreiben
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Serialize)]
struct Row {
    file: String,
    page: u32,
    chars: usize,
    lines: usize,
    garbage_ratio: f32,
    plausible_word_ratio: f32,
    decision: &'static str,
    ms: f64,
}

pub fn decision_label(decision: PageDecision) -> &'static str {
    match decision {
        PageDecision::TextLayer => "textebene",
        PageDecision::Ocr(OcrReason::NoText) => "ocr:kein-text",
        PageDecision::Ocr(OcrReason::Garbage) => "ocr:kaputt-kodiert",
        PageDecision::Ocr(OcrReason::Implausible) => "ocr:unplausibel",
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let inputs = collect(&args.inputs, false)?;
    let engine = load_pdf_engine(args.pdfium_dir.as_deref())?;
    info!("{} PDF-Dateien", inputs.len());

    let started = Instant::now();
    let mut rows = Vec::new();
    for input in &inputs {
        let file = input.name();
        let doc = match engine.open(&input.path) {
            Ok(doc) => doc,
            Err(e) => {
                warn!("{file}: {e}");
                continue;
            }
        };
        let pages = doc.page_count();
        for page in 0..pages {
            let t = Instant::now();
            let (layer, stats) = match doc.text_layer(page) {
                Ok(result) => result,
                Err(e) => {
                    warn!("{file} S. {}: {e}", page + 1);
                    continue;
                }
            };
            let lines = group_lines(layer.tokens).len();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let decision = decision_label(stats.decision());
            info!(
                "{file} S. {}/{pages}: {decision:<18} {:>5} Zeichen {:>4} Zeilen  Müll {:>4.1} %  plausibel {:>3.0} %  {ms:>6.1} ms",
                page + 1,
                stats.chars,
                lines,
                stats.garbage_ratio * 100.0,
                stats.plausible_word_ratio * 100.0,
            );
            rows.push(Row {
                file: file.clone(),
                page: page + 1,
                chars: stats.chars,
                lines,
                garbage_ratio: stats.garbage_ratio,
                plausible_word_ratio: stats.plausible_word_ratio,
                decision,
                ms,
            });
        }
    }

    let secs = started.elapsed().as_secs_f64();
    let total = rows.len();
    let count = |label: &str| rows.iter().filter(|r| r.decision == label).count();
    let pct = |n: usize| {
        if total == 0 {
            0.0
        } else {
            n as f64 * 100.0 / total as f64
        }
    };
    let text_layer = count("textebene");
    println!();
    println!(
        "{} Dateien, {total} Seiten in {secs:.1} s ({:.0} Seiten/s)",
        inputs.len(),
        total as f64 / secs.max(1e-9)
    );
    println!("Textebene nutzbar: {text_layer} ({:.0} %)", pct(text_layer));
    println!(
        "OCR nötig:         {} ({:.0} %) – keine Textebene {}, kaputt kodiert {}, unplausibel {}",
        total - text_layer,
        pct(total - text_layer),
        count("ocr:kein-text"),
        count("ocr:kaputt-kodiert"),
        count("ocr:unplausibel"),
    );

    if let Some(path) = args.json {
        std::fs::write(&path, serde_json::to_vec_pretty(&rows)?)?;
        info!("JSON geschrieben: {}", path.display());
    }
    Ok(())
}
