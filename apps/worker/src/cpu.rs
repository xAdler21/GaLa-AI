//! Rolle `cpu`: Netzlaufwerk regelmäßig scannen und `extract`-Aufgaben bearbeiten.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use ingest::extract::{self, PdfExtract};
use ingest::store::{self, SyncReport};
use ingest::{ChunkConfig, DocKind, IngestError, PageText, Task, TaskKind};
use pdf::PdfEngine;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::actor::Actor;
use crate::queue::{Handler, error_chain, sleep_or_stop};

/// Gleicht das Netzlaufwerk regelmäßig mit der Datenbank ab (legt `extract`-Aufgaben an).
pub async fn scan_loop(pool: PgPool, root: PathBuf, interval: Duration, stop: CancellationToken) {
    tracing::info!(wurzel = %root.display(), intervall_s = interval.as_secs(), "Archiv-Scan gestartet");
    while !stop.is_cancelled() {
        let started = std::time::Instant::now();
        match store::sync(&pool, &root).await {
            Ok(r) => log_report(&r, started.elapsed()),
            Err(err @ IngestError::EmptyArchive(_)) => {
                tracing::error!(%err, "Scan abgebrochen");
            }
            Err(err) => tracing::error!(error = %error_chain(&err), "Scan fehlgeschlagen"),
        }
        sleep_or_stop(interval, &stop).await;
    }
    tracing::info!("Archiv-Scan beendet");
}

fn log_report(r: &SyncReport, took: Duration) {
    tracing::info!(
        gescannt = r.scanned,
        neu = r.new,
        geaendert = r.changed,
        nur_zeitstempel = r.unchanged_touched,
        geloescht = r.deleted,
        fehler = r.errors,
        dauer_s = took.as_secs(),
        "Archiv-Scan abgeschlossen"
    );
}

pub struct CpuHandler {
    pub pool: PgPool,
    pub archive_root: PathBuf,
    pub pdf: Arc<Actor<PdfEngine>>,
    pub chunking: ChunkConfig,
}

impl Handler for CpuHandler {
    fn kinds(&self) -> &'static [TaskKind] {
        &[TaskKind::Extract]
    }

    async fn handle(&self, task: &Task) -> anyhow::Result<()> {
        match task.kind {
            TaskKind::Extract => self.extract(task).await,
            other => Err(anyhow!(
                "Aufgabenart {} gehört nicht zur Rolle cpu",
                other.as_str()
            )),
        }
    }
}

impl CpuHandler {
    async fn extract(&self, task: &Task) -> anyhow::Result<()> {
        let id = task.document_id;
        let Some(info) = store::document_info(&self.pool, id).await? else {
            tracing::info!(dokument = %id, "Dokument existiert nicht mehr, Aufgabe entfällt");
            return Ok(());
        };
        store::set_extracting(&self.pool, id).await?;
        // Bei Wiederholung: Reste eines früheren Versuchs entfernen.
        store::delete_chunks(&self.pool, id, None).await?;
        store::delete_followup_tasks(&self.pool, id).await?;

        let path = self.archive_root.join(&info.rel_path);
        let (pages, ocr_pages): (Vec<PageText>, Vec<u32>) = match info.kind {
            DocKind::Pdf => {
                let p = path.clone();
                let PdfExtract {
                    page_count,
                    text_pages,
                    ocr_pages,
                } = self
                    .pdf
                    .call(move |engine| {
                        extract::extract_pdf(engine, &p).map_err(|e| error_chain(&e))
                    })
                    .await?
                    .map_err(|m| anyhow!("{}: {m}", info.rel_path))?;
                store::set_pages(&self.pool, id, page_count).await?;
                (text_pages, ocr_pages)
            }
            kind => {
                let p = path.clone();
                let pages = tokio::task::spawn_blocking(move || {
                    match kind {
                        DocKind::Xlsx => extract::extract_xlsx(&p),
                        DocKind::Gaeb => extract::extract_gaeb(&p),
                        _ => extract::extract_text(&p),
                    }
                    .map_err(|e| error_chain(&e))
                })
                .await
                .context("Extraktions-Thread abgestürzt")?
                .map_err(|m| anyhow!("{}: {m}", info.rel_path))?;
                (pages, Vec::new())
            }
        };

        let chunks = ingest::chunk_pages(&pages, &self.chunking);
        let n = store::insert_chunks(&self.pool, id, &chunks).await?;
        for page in &ocr_pages {
            store::enqueue(&self.pool, id, TaskKind::OcrPage, Some(*page), 0).await?;
        }
        if n > 0 {
            store::enqueue_embed_once(&self.pool, id).await?;
        }
        tracing::info!(
            dokument = %info.rel_path,
            chunks = n,
            ocr_seiten = ocr_pages.len(),
            "Text extrahiert"
        );
        Ok(())
    }
}
