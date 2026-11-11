//! GaLa-AI Worker. Rollen:
//! - `cpu` (gala-coordinator): Netzlaufwerk scannen, Text extrahieren, chunken (`extract`).
//! - `vision` (gala-ocr, Feature `vision`): OCR (`ocr_page`) und Embeddings (`embed`).
//!
//! Die Rollen sprechen nur über Postgres (Tabelle `index_task`) und das Netzlaufwerk.

mod actor;
mod cpu;
#[cfg_attr(not(feature = "vision"), allow(dead_code))]
mod embed;
mod queue;
mod status;
#[cfg(feature = "vision")]
mod vision;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use clap::{Parser, ValueEnum};
use ingest::ChunkConfig;
use ingest::store::QueueConfig;
use pdf::PdfEngine;
use tokio_util::sync::CancellationToken;

use crate::actor::Actor;
use crate::queue::Ctx;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Role {
    /// Scan, Textextraktion, Chunking.
    Cpu,
    /// OCR und Embeddings (GPU).
    Vision,
}

#[derive(Parser)]
#[command(name = "worker", version, about = "GaLa-AI Worker (Dokumenten-Index)")]
struct Args {
    /// Rolle dieses Workers.
    #[arg(long, value_enum, default_value_t = Role::Cpu, env = "GALA_WORKER_ROLE")]
    role: Role,
    /// Postgres-Verbindung.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,
    /// Wurzel des Netzlaufwerks.
    #[arg(long, env = "GALA_ARCHIV_ROOT", default_value = "/mnt/archiv")]
    archiv_root: PathBuf,
    /// Datei-Store (derzeit nur für Logs/Zukunft; der Worker schreibt dort noch nichts).
    #[arg(long, env = "GALA_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Name dieses Workers in `index_task.worker`. Standard: Hostname.
    #[arg(long, env = "GALA_WORKER_NAME")]
    name: Option<String>,
    /// Abstand der Archiv-Scans (nur Rolle cpu), z. B. `10m`, `90s`, `1h`.
    #[arg(long, default_value = "10m", value_parser = parse_duration)]
    scan_interval: Duration,
    /// Parallele Aufgaben dieses Prozesses.
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    /// Verzeichnis mit der PDFium-Bibliothek (sonst Systembibliothek).
    #[arg(long, env = "PDFIUM_DIR")]
    pdfium_dir: Option<PathBuf>,
    /// OCR-Gerät: cpu, cuda oder cuda:N (nur Rolle vision).
    #[cfg(feature = "vision")]
    #[arg(long, env = "GALA_OCR_DEVICE", default_value = "cuda", value_parser = vision::parse_device)]
    ocr_device: ocr::Device,
    /// OCR-Modellkombination (nur Rolle vision).
    #[cfg(feature = "vision")]
    #[arg(long, env = "GALA_OCR_PRESET", default_value = "v5-latin", value_parser = vision::parse_preset)]
    ocr_preset: ocr::Preset,
    /// Verzeichnis mit den OCR-Modelldateien (sonst `$OAR_HOME`).
    #[cfg(feature = "vision")]
    #[arg(long, env = "GALA_OCR_MODEL_DIR")]
    ocr_model_dir: Option<PathBuf>,
    /// Ablage des Embedding-Modells (Download beim ersten Start).
    #[cfg(feature = "vision")]
    #[arg(long, env = "GALA_EMBED_CACHE_DIR")]
    embed_cache_dir: Option<PathBuf>,
}

/// `90`, `90s`, `10m`, `2h` (ohne Einheit: Sekunden). Mindestens 1 Sekunde.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("ungültige Dauer {s:?} (erwartet z. B. 10m, 90s, 1h)"))?;
    if n == 0 {
        return Err("Dauer muss größer als 0 sein".into());
    }
    Ok(Duration::from_secs(n * mult))
}

/// Hostname ohne Zusatz-Crate: Umgebung, dann `/etc/hostname`.
fn hostname() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "worker".to_string())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("Beenden angefordert, laufende Aufgaben werden abgeschlossen");
}

fn load_pdf_actor(dir: Option<PathBuf>) -> anyhow::Result<Arc<Actor<PdfEngine>>> {
    let actor = Actor::spawn("pdfium", move || {
        PdfEngine::load(dir.as_deref()).map_err(|e| anyhow!(queue::error_chain(&e)))
    })
    .context("PDFium nicht ladbar (PDFIUM_DIR setzen)")?;
    Ok(Arc::new(actor))
}

fn check_archive_root(root: &Path) -> anyhow::Result<()> {
    if !root.is_dir() {
        bail!(
            "Archiv-Wurzel {} ist kein Verzeichnis (Netzlaufwerk nicht eingebunden? GALA_ARCHIV_ROOT prüfen)",
            root.display()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let args = Args::parse();

    #[cfg(not(feature = "vision"))]
    if args.role == Role::Vision {
        bail!(
            "Die Rolle vision ist in diesem Build nicht enthalten. \
             Mit `cargo build -p worker --features vision` neu bauen (benötigt ONNX Runtime/CUDA)."
        );
    }

    let database_url = args
        .database_url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .context("DATABASE_URL fehlt (Umgebungsvariable oder --database-url)")?;
    if args.concurrency == 0 {
        bail!("--concurrency muss mindestens 1 sein");
    }
    check_archive_root(&args.archiv_root)?;

    let name = args
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(hostname);
    tracing::info!(
        worker = %name,
        rolle = ?args.role,
        archiv = %args.archiv_root.display(),
        daten = ?args.data_dir,
        parallel = args.concurrency,
        "Worker startet"
    );

    let pool = db::connect(database_url).await?;
    db::migrate(&pool).await?;

    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move {
            shutdown_signal().await;
            stop.cancel();
        }
    });

    let ctx = Arc::new(Ctx {
        pool: pool.clone(),
        name,
        queue: QueueConfig::default(),
    });
    let pdf = load_pdf_actor(args.pdfium_dir.clone())?;

    match args.role {
        Role::Cpu => {
            let handler = Arc::new(cpu::CpuHandler {
                pool: pool.clone(),
                archive_root: args.archiv_root.clone(),
                pdf,
                chunking: ChunkConfig::default(),
            });
            let scan = tokio::spawn(cpu::scan_loop(
                pool,
                args.archiv_root,
                args.scan_interval,
                stop.clone(),
            ));
            let beat = status::spawn(
                ctx.pool.clone(),
                status::WorkerInfo {
                    name: ctx.name.clone(),
                    role: "cpu",
                    ocr_device: None,
                },
                stop.clone(),
            );
            queue::run(ctx, handler, args.concurrency, stop).await;
            let _ = scan.await;
            let _ = beat.await;
        }
        Role::Vision => {
            #[cfg(feature = "vision")]
            {
                let handler = Arc::new(
                    vision::VisionHandler::new(
                        pool,
                        args.archiv_root,
                        pdf,
                        vision::VisionOptions {
                            device: args.ocr_device,
                            preset: args.ocr_preset,
                            ocr_model_dir: args.ocr_model_dir,
                            embed_cache_dir: args.embed_cache_dir,
                        },
                    )
                    .await?,
                );
                // Erst nach dem Laden melden: scheitert die OCR-Engine (z. B. CUDA nicht
                // ladbar), erscheint der Worker im Admin-Panel gar nicht erst als aktiv.
                let beat = status::spawn(
                    ctx.pool.clone(),
                    status::WorkerInfo {
                        name: ctx.name.clone(),
                        role: "vision",
                        ocr_device: Some(args.ocr_device.to_string()),
                    },
                    stop.clone(),
                );
                queue::run(ctx, handler, args.concurrency, stop).await;
                let _ = beat.await;
            }
            #[cfg(not(feature = "vision"))]
            {
                let _ = (ctx, pdf);
                unreachable!("oben abgefangen");
            }
        }
    }
    tracing::info!("Worker beendet");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("x").is_err());
        assert!(parse_duration("").is_err());
    }
}
