//! Rolle `vision` (OCR-VM): OCR von Seiten ohne Textebene und Embeddings (BGE-M3).

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use doc_ir::Page;
use ingest::extract::ocr_page_to_text;
use ingest::{Chunk, ChunkConfig, DocKind, Task, TaskKind, chunk_text, store};
use ocr::{Device, OcrEngine, PaddleOcr, PageImage, Preset};
use pdf::PdfEngine;
use retrieval::{Embedder, FastEmbedder, PgVectorStore};
use sqlx::PgPool;

use crate::actor::Actor;
use crate::embed::embed_document;
use crate::queue::{Handler, error_chain};

/// Auflösung, mit der Seiten für die OCR gerendert werden.
const OCR_DPI: u32 = 300;

/// `cpu`, `cuda` oder `cuda:N`.
pub fn parse_device(s: &str) -> Result<Device, String> {
    match s {
        "cpu" => Ok(Device::Cpu),
        "cuda" => Ok(Device::Cuda { device_id: 0 }),
        _ => s
            .strip_prefix("cuda:")
            .and_then(|n| n.parse().ok())
            .map(|device_id| Device::Cuda { device_id })
            .ok_or_else(|| "erwartet cpu, cuda oder cuda:N".to_string()),
    }
}

pub fn parse_preset(s: &str) -> Result<Preset, String> {
    Preset::from_str(s).map_err(|e| e.to_string())
}

pub struct VisionOptions {
    pub device: Device,
    pub preset: Preset,
    pub ocr_model_dir: Option<PathBuf>,
    pub embed_cache_dir: Option<PathBuf>,
}

pub struct VisionHandler {
    pool: PgPool,
    archive_root: PathBuf,
    pdf: Arc<Actor<PdfEngine>>,
    ocr: Actor<PaddleOcr>,
    embedder: Arc<dyn Embedder>,
    store: PgVectorStore,
    chunking: ChunkConfig,
}

impl VisionHandler {
    /// Lädt OCR-Modelle und Embedding-Modell (blockierend, dauert beim ersten Start länger).
    pub async fn new(
        pool: PgPool,
        archive_root: PathBuf,
        pdf: Arc<Actor<PdfEngine>>,
        opts: VisionOptions,
    ) -> anyhow::Result<Self> {
        tracing::info!(preset = opts.preset.name(), geraet = ?opts.device, "OCR-Modelle werden geladen");
        let VisionOptions {
            device,
            preset,
            ocr_model_dir,
            embed_cache_dir,
        } = opts;
        let ocr = Actor::spawn("ocr", move || {
            PaddleOcr::new(preset, device, ocr_model_dir).map_err(|e| anyhow!(e.to_string()))
        })
        .context("OCR-Engine nicht ladbar")?;

        tracing::info!("Embedding-Modell (BGE-M3) wird geladen");
        let embedder = tokio::task::spawn_blocking(move || FastEmbedder::new(embed_cache_dir))
            .await
            .context("Lade-Thread abgestürzt")?
            .map_err(|e| anyhow!(e.to_string()))
            .context("Embedding-Modell nicht ladbar")?;
        Ok(Self {
            store: PgVectorStore::new(pool.clone()),
            pool,
            archive_root,
            pdf,
            ocr,
            embedder: Arc::new(embedder),
            chunking: ChunkConfig::default(),
        })
    }

    async fn ocr_page(&self, task: &Task) -> anyhow::Result<()> {
        let page = task.page.context("ocr_page ohne Seitennummer")?;
        let id = task.document_id;
        let Some(info) = store::document_info(&self.pool, id).await? else {
            return Ok(());
        };
        if info.kind != DocKind::Pdf {
            return Err(anyhow!(
                "OCR nur für PDF möglich, nicht für {}",
                info.kind.as_str()
            ));
        }
        let path = self.archive_root.join(&info.rel_path);
        let index = page.saturating_sub(1);

        let image = self
            .pdf
            .call(move |engine| {
                let doc = engine.open(&path).map_err(|e| error_chain(&e))?;
                doc.render(index, OCR_DPI).map_err(|e| error_chain(&e))
            })
            .await?
            .map_err(|m| anyhow!("Seite {page} nicht renderbar: {m}"))?;

        let pages: Vec<Page> = self
            .ocr
            .call(move |engine| {
                engine
                    .recognize(vec![PageImage {
                        page: index,
                        dpi: OCR_DPI,
                        image,
                    }])
                    .map_err(|e| e.to_string())
            })
            .await?
            .map_err(|m| anyhow!(m))?;

        let text = pages.first().map(ocr_page_to_text).unwrap_or_default();
        let chunks: Vec<Chunk> = chunk_text(&text, &self.chunking)
            .into_iter()
            .map(|text| Chunk {
                page: Some(page),
                text,
            })
            .collect();
        // Idempotent bei Wiederholung: Chunks dieser Seite zuerst entfernen.
        store::delete_chunks(&self.pool, id, Some(page)).await?;
        let n = store::insert_chunks(&self.pool, id, &chunks).await?;
        if n > 0 {
            store::enqueue_embed_once(&self.pool, id).await?;
        }
        tracing::info!(dokument = %info.rel_path, seite = page, chunks = n, "OCR abgeschlossen");
        Ok(())
    }
}

impl Handler for VisionHandler {
    fn kinds(&self) -> &'static [TaskKind] {
        &[TaskKind::OcrPage, TaskKind::Embed]
    }

    async fn handle(&self, task: &Task) -> anyhow::Result<()> {
        match task.kind {
            TaskKind::OcrPage => self.ocr_page(task).await,
            TaskKind::Embed => {
                let n = embed_document(&self.store, &self.embedder, task.document_id).await?;
                tracing::info!(dokument = %task.document_id, chunks = n, "Embeddings gespeichert");
                Ok(())
            }
            TaskKind::Extract => Err(anyhow!("extract gehört nicht zur Rolle vision")),
        }
    }
}
