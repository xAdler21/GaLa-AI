//! Preis-Historie (Tabelle `price_ref`, befüllt vom Worker aus GAEB-Abgaben im Archiv).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// Treffer zu einem Suchtext; `query_idx` ist der 1-basierte Index in der Suchliste.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PriceHit {
    pub query_idx: i32,
    pub ep: String,
    pub menge: Option<String>,
    pub einheit: Option<String>,
    pub kurztext: String,
    pub lv_name: String,
    pub phase: i16,
    pub rel_path: String,
    pub mtime: DateTime<Utc>,
    pub similarity: f32,
}

/// Je Suchtext die `per_query` ähnlichsten Kurztexte (Trigramm-Ähnlichkeit ab 0,3), in einer
/// Abfrage für das ganze LV. Gleicher Kurztext mit gleichem Preis (GAEB und PDF desselben
/// Angebots, mehrere Fassungen) zählt nur einmal, mit dem neuesten Dokument.
pub async fn similar(
    pool: &PgPool,
    texts: &[String],
    per_query: i64,
) -> Result<Vec<PriceHit>, sqlx::Error> {
    sqlx::query_as::<_, PriceHit>(
        "SELECT q.ord::int4 AS query_idx, r.ep, r.menge, r.einheit, r.kurztext, r.lv_name, r.phase, \
                r.rel_path, r.mtime, r.similarity \
         FROM unnest($1::text[]) WITH ORDINALITY AS q(t, ord) \
         CROSS JOIN LATERAL ( \
             SELECT * FROM ( \
                 SELECT DISTINCT ON (p.kurztext, p.ep, p.einheit) \
                        p.ep::text AS ep, p.menge::text AS menge, p.einheit, p.kurztext, p.lv_name, p.phase, \
                        d.rel_path, d.mtime, similarity(p.kurztext, q.t)::float4 AS similarity \
                 FROM price_ref p JOIN document d ON d.id = p.document_id \
                 WHERE p.kurztext % q.t \
                 ORDER BY p.kurztext, p.ep, p.einheit, d.mtime DESC \
             ) u \
             ORDER BY similarity DESC, mtime DESC \
             LIMIT $2 \
         ) r",
    )
    .bind(texts)
    .bind(per_query)
    .fetch_all(pool)
    .await
}

/// Anzahl der Preise und Dokumente in der Historie (für das Admin-Panel).
pub async fn stats(pool: &PgPool) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as("SELECT count(*), count(DISTINCT document_id) FROM price_ref")
        .fetch_one(pool)
        .await
}
