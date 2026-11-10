//! `embed`-Aufgaben: Chunks ohne Vektor einbetten und speichern.

use std::sync::Arc;

use anyhow::Context;
use retrieval::{Embedder, PgVectorStore};
use uuid::Uuid;

/// Chunks pro Runde (Datenbank- und Modell-Batch).
const BATCH: i64 = 32;

pub async fn embed_document(
    store: &PgVectorStore,
    embedder: &Arc<dyn Embedder>,
    document_id: Uuid,
) -> anyhow::Result<usize> {
    let mut total = 0;
    loop {
        let pending = store
            .pending_chunks(document_id, BATCH)
            .await
            .context("Chunks ohne Embedding nicht lesbar")?;
        if pending.is_empty() {
            return Ok(total);
        }
        let texts: Vec<String> = pending.iter().map(|c| c.text.clone()).collect();
        let embedder = embedder.clone();
        let vectors = tokio::task::spawn_blocking(move || embedder.embed(&texts))
            .await
            .context("Embedding-Thread abgestürzt")?
            .context("Embedding fehlgeschlagen")?;
        let items: Vec<_> = pending.iter().map(|c| c.id).zip(vectors).collect();
        let written = store
            .upsert_embeddings(items)
            .await
            .context("Embeddings nicht speicherbar")?;
        total += written;
        if written == 0 {
            // Chunks wurden zwischenzeitlich gelöscht (Dokument geändert): nichts mehr zu tun.
            return Ok(total);
        }
    }
}
