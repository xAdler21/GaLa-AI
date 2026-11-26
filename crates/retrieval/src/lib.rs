//! Embeddings und Hybrid-Suche (pgvector + Postgres-Volltext) über die Tabelle `chunk`.
//!
//! - [`Embedder`]: Text → Vektor. [`FastEmbedder`] (Feature `fastembed`) nutzt BGE-M3 (1024 Dim.),
//!   [`HashEmbedder`] ist deterministisch und nur für Tests gedacht.
//! - [`PgVectorStore`]: Embeddings speichern und suchen. Die Hybrid-Suche fusioniert dichte
//!   Kosinus-Suche, deutsche Volltextsuche und eine Suche über den Dateipfad (Kunde, Ort, Projekt
//!   stehen oft nur dort) per Reciprocal Rank Fusion ([`rrf_fuse`]).

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

/// Eintrag beim Auflisten eines Archiv-Ordners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    /// `false`: Unterordner.
    pub is_file: bool,
    /// Indizierte Dokumente darunter (bei Dateien 1).
    pub docs: i64,
}

/// Eigener Einheitspreis aus der Preis-Historie (Tabelle `price_ref`).
#[derive(Debug, Clone, PartialEq)]
pub struct PriceRef {
    pub id: i64,
    pub rel_path: String,
    pub lv_name: String,
    /// GAEB-Phase: 84 Angebot, 85 Nebenangebot, 86 Auftrag.
    pub phase: i16,
    pub oz: String,
    pub kurztext: String,
    /// Postgres-`numeric` als Text, z. B. „12.000“.
    pub menge: Option<String>,
    pub einheit: Option<String>,
    pub ep: String,
    /// Dateidatum als TT.MM.JJJJ.
    pub datum: String,
    pub similarity: f32,
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

    /// Eigene Preise, deren Kurztext `query` enthält (Trigramm-Wortähnlichkeit, Schwelle von
    /// pg_trgm: 0,6). Gleicher Kurztext zum gleichen Preis zählt einmal, mit dem neuesten Dokument.
    pub async fn price_history(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<PriceRef>, RetrievalError> {
        let rows = sqlx::query(
            "SELECT * FROM ( \
                 SELECT DISTINCT ON (p.kurztext, p.ep, p.einheit) \
                        p.id, d.rel_path, p.lv_name, p.phase, p.oz, p.kurztext, p.menge::text AS menge, \
                        p.einheit, p.ep::text AS ep, to_char(d.mtime, 'DD.MM.YYYY') AS datum, d.mtime, \
                        word_similarity($1, p.kurztext)::float4 AS sim \
                 FROM price_ref p JOIN document d ON d.id = p.document_id \
                 WHERE $1 <% p.kurztext \
                 ORDER BY p.kurztext, p.ep, p.einheit, d.mtime DESC \
             ) u ORDER BY sim DESC, mtime DESC LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let prices = rows
            .iter()
            .map(|r| {
                Ok(PriceRef {
                    id: r.try_get("id")?,
                    rel_path: r.try_get("rel_path")?,
                    lv_name: r.try_get("lv_name")?,
                    phase: r.try_get("phase")?,
                    oz: r.try_get("oz")?,
                    kurztext: r.try_get("kurztext")?,
                    menge: r.try_get("menge")?,
                    einheit: r.try_get("einheit")?,
                    ep: r.try_get("ep")?,
                    datum: r.try_get("datum")?,
                    similarity: r.try_get("sim")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        Ok(prices)
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

    /// Hybrid-Suche, je Top-`top_n`, per RRF fusioniert, gekürzt auf `limit`:
    /// - dichte Kosinus-Suche (`query_emb`),
    /// - Volltext (`query_text`, deutsch, Wörter mit ODER verknüpft),
    /// - Pfad: die 20 Dokumente, deren Pfad die meisten Wörter der Anfrage enthält, darin die
    ///   Chunks nach Kosinus-Nähe.
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
        let emb = Vector::from(query_emb);
        let dense = sqlx::query(
            "SELECT c.id, d.rel_path, c.page, c.text \
             FROM chunk c JOIN document d ON d.id = c.document_id \
             WHERE c.emb IS NOT NULL \
             ORDER BY c.emb <=> $1 LIMIT $2",
        )
        .bind(emb.clone())
        .bind(top_n)
        .fetch_all(&self.pool)
        .await?;
        let sparse = sqlx::query(
            "SELECT c.id, d.rel_path, c.page, c.text \
             FROM chunk c JOIN document d ON d.id = c.document_id, \
                  (SELECT replace(websearch_to_tsquery('german', $1)::text, ' & ', ' | ')::tsquery) AS qq(q) \
             WHERE c.tsv @@ q \
             ORDER BY ts_rank_cd(c.tsv, q) DESC LIMIT $2",
        )
        .bind(query_text)
        .bind(top_n)
        .fetch_all(&self.pool)
        .await?;
        // shortcut: to_tsvector über alle Pfade je Anfrage (~35k Dokumente, unter 0,2 s);
        // bei deutlich mehr Dokumenten eine generierte Spalte mit GIN-Index anlegen.
        let by_path = sqlx::query(
            "WITH q AS (SELECT replace(websearch_to_tsquery('simple', $1)::text, ' & ', ' | ')::tsquery AS q), \
             docs AS ( \
                 SELECT d.id, d.rel_path, ts_rank(v, q.q) AS r \
                 FROM document d, q, \
                      to_tsvector('simple', translate(d.rel_path, '/_-.,[]()', '         ')) AS v \
                 WHERE v @@ q.q ORDER BY r DESC LIMIT 20) \
             SELECT c.id, docs.rel_path, c.page, c.text \
             FROM chunk c JOIN docs ON docs.id = c.document_id \
             WHERE c.emb IS NOT NULL \
             ORDER BY c.emb <=> $2 LIMIT $3",
        )
        .bind(query_text)
        .bind(emb)
        .bind(top_n)
        .fetch_all(&self.pool)
        .await?;

        let mut by_id: HashMap<Uuid, Row4> = HashMap::new();
        let mut lists: Vec<Vec<Uuid>> = Vec::new();
        for rows in [&dense, &sparse, &by_path] {
            let mut ids = Vec::with_capacity(rows.len());
            for r in rows {
                let row = to_row(r)?;
                ids.push(row.id);
                by_id.entry(row.id).or_insert(row);
            }
            lists.push(ids);
        }

        let fused = rrf_fuse(&lists, RRF_K);
        let path_of = |id: &Uuid| by_id.get(id).map(|r| r.rel_path.clone());
        let picked = diversify(fused, path_of, MAX_PER_DOCUMENT, limit);
        Ok(picked
            .into_iter()
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

impl PgVectorStore {
    /// Text einer Seite (`page`; ohne Seite: Dokumentanfang), Chunks in Reihenfolge,
    /// höchstens `max_chars` Zeichen. `None`, wenn Pfad oder Seite unbekannt sind.
    pub async fn page_text(
        &self,
        rel_path: &str,
        page: Option<u32>,
        max_chars: usize,
    ) -> Result<Option<Hit>, RetrievalError> {
        let page = page.and_then(|p| i32::try_from(p).ok());
        let rows = sqlx::query(
            "SELECT c.id, d.rel_path, c.page, c.text \
             FROM chunk c JOIN document d ON d.id = c.document_id \
             WHERE d.rel_path = $1 AND ($2::int IS NULL OR c.page = $2 OR c.page IS NULL) \
             ORDER BY c.ord LIMIT 40",
        )
        .bind(rel_path)
        .bind(page)
        .fetch_all(&self.pool)
        .await?;
        let mut first: Option<Row4> = None;
        let mut text = String::new();
        for r in &rows {
            let row = to_row(r)?;
            if text.chars().count() >= max_chars {
                break;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&row.text);
            first.get_or_insert(row);
        }
        Ok(first.map(|r| Hit {
            chunk_id: r.id,
            rel_path: r.rel_path,
            page: r.page.and_then(|p| u32::try_from(p).ok()),
            text: text.chars().take(max_chars).collect(),
            score: 0.0,
        }))
    }

    /// Unterordner und Dateien direkt unter `prefix` (relativ zur Archivwurzel, `""` = oben),
    /// nach Name sortiert, höchstens `limit`.
    pub async fn list_folder(
        &self,
        prefix: &str,
        limit: i64,
    ) -> Result<Vec<FolderEntry>, RetrievalError> {
        let prefix = folder_prefix(prefix);
        let rows = sqlx::query(
            "SELECT split_part(substr(rel_path, length($1) + 1), '/', 1) AS name, \
                    count(*) AS docs, \
                    bool_and(strpos(substr(rel_path, length($1) + 1), '/') = 0) AS is_file \
             FROM document WHERE starts_with(rel_path, $1) \
             GROUP BY 1 ORDER BY 1 LIMIT $2",
        )
        .bind(&prefix)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(FolderEntry {
                    name: r.try_get("name")?,
                    docs: r.try_get("docs")?,
                    is_file: r.try_get("is_file")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(Into::into)
    }
}

/// `Angebote/2019` → `Angebote/2019/`; leer oder `/` → `""`.
pub fn folder_prefix(path: &str) -> String {
    let p = path.trim().trim_matches('/');
    if p.is_empty() {
        String::new()
    } else {
        format!("{p}/")
    }
}

/// Höchstens so viele Fundstellen aus einem Dokument, damit nicht acht Seiten derselben
/// Ausschreibung die Quellen füllen.
pub const MAX_PER_DOCUMENT: usize = 2;

/// Nimmt die besten `limit` Treffer, je Dokument (`doc_of`) höchstens `per_doc`.
pub fn diversify<K, D: Hash + Eq>(
    ranked: Vec<(K, f32)>,
    doc_of: impl Fn(&K) -> Option<D>,
    per_doc: usize,
    limit: usize,
) -> Vec<(K, f32)> {
    let mut seen: HashMap<D, usize> = HashMap::new();
    ranked
        .into_iter()
        .filter(|(k, _)| {
            let Some(d) = doc_of(k) else { return false };
            let n = seen.entry(d).or_insert(0);
            *n += 1;
            *n <= per_doc
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diversify_caps_per_document() {
        let ranked = vec![
            ("a1", 0.9),
            ("a2", 0.8),
            ("a3", 0.7),
            ("b1", 0.6),
            ("c1", 0.5),
        ];
        let out = diversify(ranked, |k| Some(k.as_bytes()[0]), 2, 3);
        let keys: Vec<_> = out.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["a1", "a2", "b1"]);
    }

    #[test]
    fn folder_prefix_normalizes() {
        assert_eq!(folder_prefix(""), "");
        assert_eq!(folder_prefix(" / "), "");
        assert_eq!(folder_prefix("/Angebote/2019/"), "Angebote/2019/");
        assert_eq!(folder_prefix("Angebote"), "Angebote/");
    }

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
