//! Embeddings und Hybrid-Suche (pgvector + Postgres-Volltext) über die Tabelle `chunk`.
//!
//! - [`Embedder`]: Text → Vektor. [`FastEmbedder`] (Feature `fastembed`) nutzt BGE-M3 (1024 Dim.),
//!   [`HashEmbedder`] ist deterministisch und nur für Tests gedacht.
//! - [`PgVectorStore`]: Embeddings speichern und suchen. Die Hybrid-Suche fusioniert dichte
//!   Kosinus-Suche und deutsche Volltextsuche per Reciprocal Rank Fusion ([`rrf_fuse`]).

use std::collections::HashMap;
use std::hash::Hash;

use pgvector::Vector;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Dimension von BGE-M3 und der Spalte `chunk.emb`.
pub const EMBEDDING_DIM: usize = 1024;

/// Üblicher RRF-Dämpfungsfaktor.
pub const RRF_K: f32 = 60.0;

#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    #[error("Datenbankfehler")]
    Db(#[from] sqlx::Error),
    #[error("Embedding fehlgeschlagen: {0}")]
    Embed(String),
    #[error("Embedding hat {got} statt {expected} Dimensionen")]
    Dim { expected: usize, got: usize },
}

/// Wandelt Texte in Vektoren um. Blockierend (CPU/GPU-Inferenz); aus async-Code über
/// `spawn_blocking` aufrufen, wenn der Aufruf lange dauern kann.
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    /// Ein Vektor je Text, gleiche Reihenfolge.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RetrievalError>;
}

/// Deterministischer Pseudo-Embedder (Hashing-Trick über Wörter, L2-normiert). Nur für Tests:
/// Texte mit gemeinsamen Wörtern liegen nah beieinander.
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(EMBEDDING_DIM)
    }
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RetrievalError> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0.0f32; self.dim];
                for word in text
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                {
                    // FNV-1a, damit das Ergebnis über Plattformen und Läufe stabil bleibt.
                    let mut h: u64 = 0xcbf29ce484222325;
                    for b in word.to_lowercase().bytes() {
                        h ^= u64::from(b);
                        h = h.wrapping_mul(0x100000001b3);
                    }
                    v[(h % self.dim as u64) as usize] += 1.0;
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    v.iter_mut().for_each(|x| *x /= norm);
                }
                v
            })
            .collect())
    }
}

#[cfg(feature = "fastembed")]
mod fast {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

    use crate::{EMBEDDING_DIM, Embedder, RetrievalError};

    /// BGE-M3 (dichter Anteil, 1024 Dim.) über fastembed / ONNX Runtime.
    pub struct FastEmbedder {
        model: Mutex<TextEmbedding>,
        batch_size: usize,
    }

    impl FastEmbedder {
        /// `cache_dir`: Ablage der Modelldateien (Download beim ersten Start, falls nicht da).
        pub fn new(cache_dir: Option<PathBuf>) -> Result<Self, RetrievalError> {
            let mut opts =
                TextInitOptions::new(EmbeddingModel::BGEM3).with_show_download_progress(true);
            if let Some(dir) = cache_dir {
                opts = opts.with_cache_dir(dir);
            }
            let model =
                TextEmbedding::try_new(opts).map_err(|e| RetrievalError::Embed(e.to_string()))?;
            Ok(Self {
                model: Mutex::new(model),
                batch_size: 8,
            })
        }
    }

    impl Embedder for FastEmbedder {
        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }

        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, RetrievalError> {
            let mut model = self
                .model
                .lock()
                .map_err(|_| RetrievalError::Embed("Modell-Lock vergiftet".into()))?;
            model
                .embed(texts, Some(self.batch_size))
                .map_err(|e| RetrievalError::Embed(e.to_string()))
        }
    }
}

#[cfg(feature = "fastembed")]
pub use fast::FastEmbedder;

/// Treffer der Hybrid-Suche.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub chunk_id: Uuid,
    /// Pfad relativ zur Archivwurzel.
    pub rel_path: String,
    /// 1-basiert.
    pub page: Option<u32>,
    pub text: String,
    /// RRF-Score (nur zum Sortieren/Vergleichen sinnvoll).
    pub score: f32,
}

/// Reciprocal Rank Fusion: `score(d) = Σ 1 / (k + rang_i(d))` über alle Listen (Rang ab 1).
/// Ergebnis nach Score absteigend; bei Gleichstand gewinnt, wer zuerst in einer Liste auftaucht.
pub fn rrf_fuse<K: Hash + Eq + Clone>(lists: &[Vec<K>], k: f32) -> Vec<(K, f32)> {
    let mut scores: HashMap<K, (f32, usize)> = HashMap::new();
    let mut order = 0usize;
    for list in lists {
        for (rank, key) in list.iter().enumerate() {
            let entry = scores.entry(key.clone()).or_insert_with(|| {
                order += 1;
                (0.0, order)
            });
            entry.0 += 1.0 / (k + rank as f32 + 1.0);
        }
    }
    let mut out: Vec<(K, f32, usize)> = scores
        .into_iter()
        .map(|(key, (s, o))| (key, s, o))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    out.into_iter().map(|(key, s, _)| (key, s)).collect()
}

/// Chunk ohne Embedding (Eingabe für den `embed`-Task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChunk {
    pub id: Uuid,
    pub text: String,
}

#[derive(Clone)]
pub struct PgVectorStore {
    pool: PgPool,
}

struct Row4 {
    id: Uuid,
    rel_path: String,
    page: Option<i32>,
    text: String,
}

fn to_row(r: &sqlx::postgres::PgRow) -> Result<Row4, sqlx::Error> {
    Ok(Row4 {
        id: r.try_get("id")?,
        rel_path: r.try_get("rel_path")?,
        page: r.try_get("page")?,
        text: r.try_get("text")?,
    })
}

impl PgVectorStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Chunks eines Dokuments, die noch kein Embedding haben (höchstens `limit`).
    pub async fn pending_chunks(
        &self,
        document_id: Uuid,
        limit: i64,
    ) -> Result<Vec<PendingChunk>, RetrievalError> {
        let rows = sqlx::query("SELECT id, text FROM chunk WHERE document_id = $1 AND emb IS NULL ORDER BY ord LIMIT $2")
            .bind(document_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(PendingChunk {
                    id: r.try_get("id")?,
                    text: r.try_get("text")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(Into::into)
    }

    /// Schreibt Embeddings (idempotent: überschreibt vorhandene).
    pub async fn upsert_embeddings(
        &self,
        items: Vec<(Uuid, Vec<f32>)>,
    ) -> Result<usize, RetrievalError> {
        let mut tx = self.pool.begin().await?;
        let mut n = 0;
        for (id, emb) in items {
            if emb.len() != EMBEDDING_DIM {
                return Err(RetrievalError::Dim {
                    expected: EMBEDDING_DIM,
                    got: emb.len(),
                });
            }
            n += sqlx::query("UPDATE chunk SET emb = $2 WHERE id = $1")
                .bind(id)
                .bind(Vector::from(emb))
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(n)
    }

    /// Hybrid-Suche: dichte Kosinus-Suche (`query_emb`) und Volltext (`query_text`, deutsch,
    /// `websearch_to_tsquery`), je Top-`top_n`, per RRF fusioniert, gekürzt auf `limit`.
    pub async fn hybrid_search(
        &self,
        query_text: &str,
        query_emb: Vec<f32>,
        top_n: i64,
        limit: usize,
    ) -> Result<Vec<Hit>, RetrievalError> {
        if query_emb.len() != EMBEDDING_DIM {
            return Err(RetrievalError::Dim {
                expected: EMBEDDING_DIM,
                got: query_emb.len(),
            });
        }
        let dense = sqlx::query(
            "SELECT c.id, d.rel_path, c.page, c.text \
             FROM chunk c JOIN document d ON d.id = c.document_id \
             WHERE c.emb IS NOT NULL \
             ORDER BY c.emb <=> $1 LIMIT $2",
        )
        .bind(Vector::from(query_emb))
        .bind(top_n)
        .fetch_all(&self.pool)
        .await?;
        let sparse = sqlx::query(
            "SELECT c.id, d.rel_path, c.page, c.text \
             FROM chunk c JOIN document d ON d.id = c.document_id, \
                  websearch_to_tsquery('german', $1) q \
             WHERE c.tsv @@ q \
             ORDER BY ts_rank_cd(c.tsv, q) DESC LIMIT $2",
        )
        .bind(query_text)
        .bind(top_n)
        .fetch_all(&self.pool)
        .await?;

        let mut by_id: HashMap<Uuid, Row4> = HashMap::new();
        let mut lists: Vec<Vec<Uuid>> = Vec::new();
        for rows in [&dense, &sparse] {
            let mut ids = Vec::with_capacity(rows.len());
            for r in rows {
                let row = to_row(r)?;
                ids.push(row.id);
                by_id.entry(row.id).or_insert(row);
            }
            lists.push(ids);
        }

        Ok(rrf_fuse(&lists, RRF_K)
            .into_iter()
            .take(limit)
            .filter_map(|(id, score)| {
                let r = by_id.remove(&id)?;
                Some(Hit {
                    chunk_id: r.id,
                    rel_path: r.rel_path,
                    page: r.page.and_then(|p| u32::try_from(p).ok()),
                    text: r.text,
                    score,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_single_list_keeps_order() {
        let fused = rrf_fuse(&[vec!["a", "b", "c"]], RRF_K);
        let keys: Vec<_> = fused.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert!((fused[0].1 - 1.0 / 61.0).abs() < 1e-6);
    }

    #[test]
    fn rrf_rewards_agreement() {
        // "b" ist in beiden Listen vorn-ish und schlägt "a" (nur in einer Liste auf Platz 1).
        let fused = rrf_fuse(&[vec!["a", "b"], vec!["b", "c"]], RRF_K);
        let keys: Vec<_> = fused.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["b", "a", "c"]);
        let expected_b = 1.0 / 62.0 + 1.0 / 61.0;
        assert!((fused[0].1 - expected_b).abs() < 1e-6);
    }

    #[test]
    fn rrf_tie_is_stable() {
        let fused = rrf_fuse(&[vec!["x"], vec!["y"]], RRF_K);
        let keys: Vec<_> = fused.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["x", "y"]);
    }

    #[test]
    fn rrf_empty() {
        assert!(rrf_fuse::<u32>(&[], RRF_K).is_empty());
        assert!(rrf_fuse::<u32>(&[vec![], vec![]], RRF_K).is_empty());
    }

    #[test]
    fn hash_embedder_is_deterministic_and_normalized() {
        let e = HashEmbedder::new(64);
        let a = e.embed(&["Betonpflaster verlegen".into()]).unwrap();
        let b = e.embed(&["Betonpflaster verlegen".into()]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), 64);
        let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn hash_embedder_similar_texts_are_closer() {
        let e = HashEmbedder::new(256);
        let v = e
            .embed(&[
                "Pflaster im Sandbett verlegen".into(),
                "Pflaster verlegen Sandbett".into(),
                "Rechnung Dachdecker Mai".into(),
            ])
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]));
    }
}
