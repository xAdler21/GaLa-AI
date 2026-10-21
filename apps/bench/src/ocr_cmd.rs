use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use ocr::{Device, OcrEngine, PaddleOcr, PageImage, Preset};
use pdf::PageDecision;
use serde::Serialize;
use tracing::{info, warn};

use crate::cer::{cer, normalize, percentile, reading_order_text};
use crate::inputs::{Input, Kind, collect, load_pdf_engine};
use crate::text_cmd::decision_label;

/// Ab dieser Konfidenz gilt ein OCR-Token als sicher (wie die Review-Schwelle für Zahlen).
const SURE_CONF: f32 = 0.9;

#[derive(clap::Args)]
pub struct Args {
    /// PDFs, Bilder (PNG/JPG/TIFF) oder Ordner (rekursiv)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// OCR-Modellkombination; mehrfach angeben zum Vergleichen
    /// (v5-latin, v5-server-latin, v6-small, v6-medium)
    #[arg(long = "preset", default_value = "v5-latin")]
    presets: Vec<Preset>,
    /// cpu, cuda oder cuda:N
    #[arg(long, default_value = "cpu", value_parser = parse_device)]
    device: Device,
    /// Verzeichnis mit den Modelldateien; ohne Angabe lädt oar-ocr sie nach $OAR_HOME
    #[arg(long)]
    model_dir: Option<PathBuf>,
    /// Verzeichnis mit der PDFium-Bibliothek
    #[arg(long, env = "PDFIUM_DIR")]
    pdfium_dir: Option<PathBuf>,
    /// Render-Auflösung für PDF-Seiten
    #[arg(long, default_value_t = 300)]
    dpi: u32,
    /// Angenommene Auflösung von Bilddateien
    #[arg(long, default_value_t = 300)]
    image_dpi: u32,
    /// Seiten pro OCR-Aufruf
    #[arg(long, default_value_t = 4)]
    batch: usize,
    /// Nur Seiten ohne brauchbare Textebene. Standard: alle Seiten, dann dient die Textebene
    /// digitaler Seiten als Referenz für die Zeichenfehlerrate.
    #[arg(long)]
    only_needed: bool,
    /// Höchstens so viele Seiten je Preset
    #[arg(long)]
    max_pages: Option<usize>,
    /// Ergebnisse zusätzlich als JSON schreiben
    #[arg(long)]
    json: Option<PathBuf>,
    /// Erkannte Texte und Referenzen zum Vergleichen ablegen
    #[arg(long)]
    dump_dir: Option<PathBuf>,
}

fn parse_device(s: &str) -> Result<Device, String> {
    match s {
        "cpu" => Ok(Device::Cpu),
        "cuda" => Ok(Device::Cuda { device_id: 0 }),
        _ => s
            .strip_prefix("cuda:")
            .and_then(|id| id.parse().ok())
            .map(|device_id| Device::Cuda { device_id })
            .ok_or_else(|| format!("unbekanntes Gerät {s:?}, erlaubt: cpu, cuda, cuda:N")),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReferenceKind {
    GroundTruth,
    TextLayer,
}

#[derive(Serialize)]
struct Row {
    preset: &'static str,
    file: String,
    page: u32,
    /// Triage der Textebene (nur bei PDFs).
    text_layer: Option<&'static str>,
    render_ms: f64,
    ocr_ms: f64,
    tokens: usize,
    mean_conf: Option<f32>,
    unsure_ratio: f32,
    cer: Option<f32>,
    reference: Option<ReferenceKind>,
}

struct Pending {
    file: String,
    text_layer: Option<&'static str>,
    render_ms: f64,
    reference: Option<(ReferenceKind, String)>,
    image: PageImage,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let inputs = collect(&args.inputs, true)?;
    let pdf = if inputs.iter().any(|i| i.kind == Kind::Pdf) {
        Some(load_pdf_engine(args.pdfium_dir.as_deref())?)
    } else {
        None
    };
    if let Some(dir) = &args.dump_dir {
        std::fs::create_dir_all(dir)?;
    }

    let mut all_rows = Vec::new();
    for &preset in &args.presets {
        info!("[{}] Modelle laden ({:?}) …", preset.name(), args.device);
        let t = Instant::now();
        let engine = PaddleOcr::new(preset, args.device, args.model_dir.clone())
            .with_context(|| format!("Preset {}", preset.name()))?;
        let load_s = t.elapsed().as_secs_f64();
        info!("[{}] bereit nach {load_s:.1} s", engine.name());

        let started = Instant::now();
        let mut rows = Vec::new();
        let mut batch = Vec::new();
        let mut budget = args.max_pages.unwrap_or(usize::MAX);

        'inputs: for input in &inputs {
            for pending in pages_of(input, pdf.as_ref(), &args) {
                if budget == 0 {
                    break 'inputs;
                }
                budget -= 1;
                batch.push(pending?);
                if batch.len() >= args.batch.max(1) {
                    rows.extend(flush(&engine, preset, &mut batch, args.dump_dir.as_ref())?);
                }
            }
        }
        rows.extend(flush(&engine, preset, &mut batch, args.dump_dir.as_ref())?);
        summarize(&engine, &rows, load_s, started.elapsed().as_secs_f64());
        all_rows.extend(rows);
    }

    if let Some(path) = args.json {
        std::fs::write(&path, serde_json::to_vec_pretty(&all_rows)?)?;
        info!("JSON geschrieben: {}", path.display());
    }
    Ok(())
}

/// Liefert die zu erkennenden Seiten eines Eingabedokuments, gerendert und mit Referenztext.
fn pages_of<'a>(
    input: &'a Input,
    pdf: Option<&'a pdf::PdfEngine>,
    args: &'a Args,
) -> Box<dyn Iterator<Item = anyhow::Result<Pending>> + 'a> {
    let file = input.name();
    match input.kind {
        Kind::Image => Box::new(std::iter::once_with(move || {
            let t = Instant::now();
            let image = image::open(&input.path)
                .with_context(|| format!("{file} nicht lesbar"))?
                .into_rgb8();
            Ok(Pending {
                render_ms: t.elapsed().as_secs_f64() * 1000.0,
                reference: input
                    .ground_truth(0)
                    .map(|text| (ReferenceKind::GroundTruth, normalize(&text))),
                text_layer: None,
                image: PageImage {
                    page: 0,
                    dpi: args.image_dpi,
                    image,
                },
                file,
            })
        })),
        Kind::Pdf => {
            let doc = match pdf.map(|engine| engine.open(&input.path)) {
                Some(Ok(doc)) => doc,
                Some(Err(e)) => {
                    warn!("{file}: {e}");
                    return Box::new(std::iter::empty());
                }
                None => return Box::new(std::iter::empty()),
            };
            let pages = doc.page_count();
            Box::new((0..pages).filter_map(move |page| {
                let result: anyhow::Result<Option<Pending>> = (|| {
                    let (layer, stats) = doc.text_layer(page)?;
                    let decision = stats.decision();
                    if args.only_needed && decision == PageDecision::TextLayer {
                        return Ok(None);
                    }
                    let reference = match input.ground_truth(page) {
                        Some(text) => Some((ReferenceKind::GroundTruth, normalize(&text))),
                        None => (decision == PageDecision::TextLayer)
                            .then(|| (ReferenceKind::TextLayer, reading_order_text(layer.tokens))),
                    };
                    let t = Instant::now();
                    let image = doc.render(page, args.dpi)?;
                    Ok(Some(Pending {
                        file: format!("{file} S. {}/{pages}", page + 1),
                        text_layer: Some(decision_label(decision)),
                        render_ms: t.elapsed().as_secs_f64() * 1000.0,
                        reference,
                        image: PageImage {
                            page,
                            dpi: args.dpi,
                            image,
                        },
                    }))
                })();
                result.transpose()
            }))
        }
    }
}

fn flush(
    engine: &PaddleOcr,
    preset: Preset,
    batch: &mut Vec<Pending>,
    dump_dir: Option<&PathBuf>,
) -> anyhow::Result<Vec<Row>> {
    if batch.is_empty() {
        return Ok(Vec::new());
    }
    let pending: Vec<Pending> = std::mem::take(batch);
    let (meta, images): (Vec<_>, Vec<_>) = pending
        .into_iter()
        .map(|p| ((p.file, p.text_layer, p.render_ms, p.reference), p.image))
        .unzip();

    let t = Instant::now();
    let pages = engine.recognize(images)?;
    let ocr_ms = t.elapsed().as_secs_f64() * 1000.0 / pages.len().max(1) as f64;

    let mut rows = Vec::new();
    for (page, (file, text_layer, render_ms, reference)) in pages.into_iter().zip(meta) {
        let mean_conf = page.mean_conf();
        let tokens = page.tokens.len();
        let unsure = page.tokens.iter().filter(|t| t.conf < SURE_CONF).count();
        let unsure_ratio = if tokens == 0 {
            0.0
        } else {
            unsure as f32 / tokens as f32
        };
        let text = reading_order_text(page.tokens);
        let error_rate = reference.as_ref().map(|(_, r)| cer(r, &text));

        info!(
            "[{}] {file}: Render {render_ms:>5.0} ms  OCR {ocr_ms:>6.0} ms  {tokens:>4} Tokens  Konfidenz {}  unsicher {:>4.1} %  CER {}",
            preset.name(),
            mean_conf.map_or("–".into(), |c| format!("{c:.3}")),
            unsure_ratio * 100.0,
            match (&reference, error_rate) {
                (Some((ReferenceKind::GroundTruth, _)), Some(e)) =>
                    format!("{:.2} % (Ground Truth)", e * 100.0),
                (Some((ReferenceKind::TextLayer, _)), Some(e)) =>
                    format!("{:.2} % (Textebene)", e * 100.0),
                _ => "–".into(),
            },
        );

        if let Some(dir) = dump_dir {
            let base = sanitize(&format!("{}_{file}", preset.name()));
            std::fs::write(dir.join(format!("{base}.ocr.txt")), &text)?;
            if let Some((_, reference)) = &reference {
                std::fs::write(dir.join(format!("{base}.ref.txt")), reference)?;
            }
        }

        rows.push(Row {
            preset: preset.name(),
            file,
            page: page.info.index + 1,
            text_layer,
            render_ms,
            ocr_ms,
            tokens,
            mean_conf,
            unsure_ratio,
            cer: error_rate,
            reference: reference.map(|(kind, _)| kind),
        });
    }
    Ok(rows)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn summarize(engine: &PaddleOcr, rows: &[Row], load_s: f64, total_s: f64) {
    let n = rows.len();
    let avg = |f: fn(&Row) -> f64| rows.iter().map(f).sum::<f64>() / n.max(1) as f64;
    let confs: Vec<f32> = rows.iter().filter_map(|r| r.mean_conf).collect();
    let cers = |kind: ReferenceKind| -> Vec<f32> {
        rows.iter()
            .filter(|r| r.reference == Some(kind))
            .filter_map(|r| r.cer)
            .collect()
    };
    let fmt_pct = |v: Option<f32>| v.map_or("–".to_string(), |x| format!("{:.2} %", x * 100.0));

    println!();
    println!("== {} ==", engine.name());
    println!(
        "Seiten: {n}, Modell-Ladezeit {load_s:.1} s, gesamt {total_s:.1} s ({:.2} Seiten/s inkl. Rendern)",
        n as f64 / total_s.max(1e-9)
    );
    println!(
        "Rendern Ø {:.0} ms/Seite, OCR Ø {:.0} ms/Seite",
        avg(|r| r.render_ms),
        avg(|r| r.ocr_ms)
    );
    println!(
        "Konfidenz Median {}, unsichere Tokens Ø {:.1} %",
        percentile(&confs, 0.5).map_or("–".into(), |c| format!("{c:.3}")),
        avg(|r| f64::from(r.unsure_ratio)) * 100.0
    );
    let mut any_reference = false;
    for (label, kind) in [
        ("gegen Ground Truth", ReferenceKind::GroundTruth),
        ("gegen Textebene", ReferenceKind::TextLayer),
    ] {
        let values = cers(kind);
        if !values.is_empty() {
            any_reference = true;
            println!(
                "Zeichenfehlerrate {label} ({} Seiten): Median {}, P90 {}",
                values.len(),
                fmt_pct(percentile(&values, 0.5)),
                fmt_pct(percentile(&values, 0.9))
            );
        }
    }
    if !any_reference {
        println!("Keine Referenz vorhanden – für Scans <name>.p<Seite>.txt neben das PDF legen.");
    }
}
