//! Einkaufspreise (Tabellen `ek_price`, `ek_scan`), gelesen aus Lieferantenbelegen im Archiv.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Bedingung für Lieferantenbelege mit Preisen: PDF im Lieferantenordner, Angebot,
/// Auftragsbestätigung oder Preisliste; Lieferscheine haben meist keine Preise.
/// Als Makro, damit die Abfragen feste Strings bleiben (`concat!`).
macro_rules! candidate {
    () => {
        "d.kind = 'pdf' AND d.status = 'indexed' AND d.rel_path ILIKE '%lieferant%' \
         AND d.rel_path ~* '(angebot|auftragsbest|bestätigung|preisliste|/AB[ _-])' \
         AND d.rel_path !~* 'lieferschein' \
         AND (s.document_id IS NULL OR s.sha256 <> d.sha256)"
    };
}

/// Ein noch nicht (oder in älterer Fassung) gelesener Beleg mit seinem Text.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Candidate {
    pub id: Uuid,
    pub rel_path: String,
    pub sha256: String,
    pub mtime: DateTime<Utc>,
    pub text: String,
}

/// Nächster Beleg, neueste Dateien zuerst. Der Text ist auf `max_chars` gekürzt.
pub async fn next_candidate(
    pool: &PgPool,
    max_chars: i32,
) -> Result<Option<Candidate>, sqlx::Error> {
    sqlx::query_as::<_, Candidate>(concat!(
        "SELECT d.id, d.rel_path, d.sha256, d.mtime, \
                left(coalesce((SELECT string_agg(c.text, E'\\n\\n' ORDER BY c.ord) FROM chunk c \
                               WHERE c.document_id = d.id), ''), $1) AS text \
         FROM document d LEFT JOIN ek_scan s ON s.document_id = d.id WHERE ",
        candidate!(),
        " ORDER BY d.mtime DESC LIMIT 1"
    ))
    .bind(max_chars)
    .fetch_optional(pool)
    .await
}

/// Ein ausgelesener Einkaufspreis. Zahlen als Text, damit sie ohne Rundung als `numeric`
/// gespeichert werden.
#[derive(Debug, Clone, PartialEq)]
pub struct EkRow {
    pub bezeichnung: String,
    pub menge: Option<String>,
    pub einheit: Option<String>,
    pub ep: String,
}

/// Ersetzt die Preise eines Belegs und merkt ihn als gelesen (auch bei Fehler, damit er nicht
/// immer wieder drankommt; mit neuer Dateifassung wird er neu gelesen).
pub async fn store(
    pool: &PgPool,
    doc: &Candidate,
    lieferant: &str,
    belegdatum: Option<NaiveDate>,
    rows: &[EkRow],
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM ek_price WHERE document_id = $1")
        .bind(doc.id)
        .execute(&mut *tx)
        .await?;
    if !rows.is_empty() {
        let col = |f: fn(&EkRow) -> Option<String>| rows.iter().map(f).collect::<Vec<_>>();
        sqlx::query(
            "INSERT INTO ek_price (document_id, lieferant, belegdatum, bezeichnung, menge, einheit, ep) \
             SELECT $1, $2, $3, u.b, u.m::numeric, u.e, u.ep::numeric \
             FROM UNNEST($4::text[], $5::text[], $6::text[], $7::text[]) AS u(b, m, e, ep)",
        )
        .bind(doc.id)
        .bind(lieferant)
        .bind(belegdatum)
        .bind(col(|r| Some(r.bezeichnung.clone())))
        .bind(col(|r| r.menge.clone()))
        .bind(col(|r| r.einheit.clone()))
        .bind(col(|r| Some(r.ep.clone())))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO ek_scan (document_id, sha256, prices, error, scanned_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (document_id) DO UPDATE SET sha256 = $2, prices = $3, error = $4, scanned_at = now()",
    )
    .bind(doc.id)
    .bind(&doc.sha256)
    .bind(i32::try_from(rows.len()).unwrap_or(i32::MAX))
    .bind(error)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Treffer zu einem Suchtext; `query_idx` ist der 1-basierte Index in der Suchliste.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EkHit {
    pub query_idx: i32,
    pub ep: String,
    pub menge: Option<String>,
    pub einheit: Option<String>,
    pub bezeichnung: String,
    pub lieferant: String,
    pub belegdatum: Option<NaiveDate>,
    pub rel_path: String,
    pub mtime: DateTime<Utc>,
    pub similarity: f32,
}

/// Je Suchtext die `per_query` ähnlichsten Artikel (Trigramm-Ähnlichkeit ab 0,3). Gleicher
/// Artikel zum gleichen Preis beim gleichen Lieferanten zählt einmal, mit dem neuesten Beleg.
pub async fn similar(
    pool: &PgPool,
    texts: &[String],
    per_query: i64,
) -> Result<Vec<EkHit>, sqlx::Error> {
    sqlx::query_as::<_, EkHit>(
        "SELECT q.ord::int4 AS query_idx, r.ep, r.menge, r.einheit, r.bezeichnung, r.lieferant, \
                r.belegdatum, r.rel_path, r.mtime, r.similarity \
         FROM unnest($1::text[]) WITH ORDINALITY AS q(t, ord) \
         CROSS JOIN LATERAL ( \
             SELECT * FROM ( \
                 SELECT DISTINCT ON (e.bezeichnung, e.ep, e.lieferant) \
                        e.ep::text AS ep, e.menge::text AS menge, e.einheit, e.bezeichnung, e.lieferant, \
                        e.belegdatum, d.rel_path, d.mtime, similarity(e.bezeichnung, q.t)::float4 AS similarity \
                 FROM ek_price e JOIN document d ON d.id = e.document_id \
                 WHERE e.bezeichnung % q.t \
                 ORDER BY e.bezeichnung, e.ep, e.lieferant, coalesce(e.belegdatum, d.mtime::date) DESC \
             ) u \
             ORDER BY similarity DESC, coalesce(belegdatum, mtime::date) DESC \
             LIMIT $2 \
         ) r",
    )
    .bind(texts)
    .bind(per_query)
    .fetch_all(pool)
    .await
}

/// Stand für das Admin-Panel: (gelesene Belege, offene Belege, Preise).
pub async fn stats(pool: &PgPool) -> Result<(i64, i64, i64), sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT (SELECT count(*) FROM ek_scan), \
                (SELECT count(*) FROM document d LEFT JOIN ek_scan s ON s.document_id = d.id WHERE ",
        candidate!(),
        "), (SELECT count(*) FROM ek_price)"
    ))
    .fetch_one(pool)
    .await
}
