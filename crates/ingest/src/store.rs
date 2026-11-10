//! Datenbankseite des Index: Dokumente synchronisieren, Chunks schreiben, Aufgabenwarteschlange.
//!
//! Die Warteschlange (`index_task`) ist bewusst klein: `FOR UPDATE SKIP LOCKED`, Lease mit
//! Heartbeat, Wiederholung mit Höchstzahl an Versuchen. Sie ist der Vorläufer des geplanten
//! `jobs`-Crates und wird darin aufgehen.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::IngestError;
use crate::chunk::Chunk;
use crate::scan::{self, DocKind, KnownDoc, ScannedFile};

// ---------------------------------------------------------------- Aufgaben

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Extract,
    OcrPage,
    Embed,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Extract => "extract",
            TaskKind::OcrPage => "ocr_page",
            TaskKind::Embed => "embed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "extract" => TaskKind::Extract,
            "ocr_page" => TaskKind::OcrPage,
            "embed" => TaskKind::Embed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    pub document_id: Uuid,
    pub kind: TaskKind,
    /// 1-basiert, nur bei `OcrPage`.
    pub page: Option<u32>,
    /// Anzahl bisheriger Versuche inklusive des laufenden.
    pub attempts: i32,
}

/// Parameter der Warteschlange.
#[derive(Debug, Clone, Copy)]
pub struct QueueConfig {
    pub lease: Duration,
    pub max_attempts: i32,
    /// Wartezeit vor Versuch n+1: `retry_backoff * n`.
    pub retry_backoff: Duration,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(120),
            max_attempts: 3,
            retry_backoff: Duration::from_secs(60),
        }
    }
}

pub async fn enqueue(
    pool: &PgPool,
    document_id: Uuid,
    kind: TaskKind,
    page: Option<u32>,
    priority: i32,
) -> Result<(), IngestError> {
    sqlx::query(
        "INSERT INTO index_task (document_id, kind, page, priority) VALUES ($1, $2, $3, $4)",
    )
    .bind(document_id)
    .bind(kind.as_str())
    .bind(page.map(|p| p as i32))
    .bind(priority)
    .execute(pool)
    .await?;
    Ok(())
}

/// Reiht einen `embed`-Task ein, falls für das Dokument keiner wartet (verhindert Stapel).
pub async fn enqueue_embed_once(pool: &PgPool, document_id: Uuid) -> Result<(), IngestError> {
    sqlx::query(
        "INSERT INTO index_task (document_id, kind) \
         SELECT $1, 'embed' WHERE NOT EXISTS \
         (SELECT 1 FROM index_task WHERE document_id = $1 AND kind = 'embed' AND status = 'pending')",
    )
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Holt die nächste Aufgabe der gewünschten Arten und setzt eine Lease.
/// Abgelaufene Leases (abgestürzter Worker) werden wieder vergeben, solange Versuche übrig sind;
/// Aufgaben mit erschöpften Versuchen werden vorher als `failed` markiert.
pub async fn claim(
    pool: &PgPool,
    kinds: &[TaskKind],
    worker: &str,
    cfg: &QueueConfig,
) -> Result<Option<Task>, IngestError> {
    sqlx::query(
        "UPDATE index_task SET status = 'failed', error = coalesce(error, 'Lease abgelaufen, Versuche erschöpft'), \
         updated_at = now() WHERE status = 'running' AND lease_until < now() AND attempts >= $1",
    )
    .bind(cfg.max_attempts)
    .execute(pool)
    .await?;

    let kinds: Vec<String> = kinds.iter().map(|k| k.as_str().to_string()).collect();
    let row = sqlx::query(
        "UPDATE index_task SET status = 'running', attempts = attempts + 1, worker = $2, \
         lease_until = now() + make_interval(secs => $3), updated_at = now() \
         WHERE id = ( \
            SELECT id FROM index_task \
            WHERE kind = ANY($1) AND attempts < $4 AND ( \
                (status = 'pending' AND not_before <= now()) OR (status = 'running' AND lease_until < now())) \
            ORDER BY priority DESC, id LIMIT 1 FOR UPDATE SKIP LOCKED) \
         RETURNING id, document_id, kind, page, attempts",
    )
    .bind(&kinds)
    .bind(worker)
    .bind(cfg.lease.as_secs_f64())
    .bind(cfg.max_attempts)
    .fetch_optional(pool)
    .await?;

    row.map(|r| {
        let kind: String = r.try_get("kind")?;
        let page: Option<i32> = r.try_get("page")?;
        Ok(Task {
            id: r.try_get("id")?,
            document_id: r.try_get("document_id")?,
            kind: TaskKind::parse(&kind)
                .ok_or_else(|| IngestError::Other(format!("unbekannte Task-Art {kind}")))?,
            page: page.and_then(|p| u32::try_from(p).ok()),
            attempts: r.try_get("attempts")?,
        })
    })
    .transpose()
}

/// Verlängert die Lease. `false`, wenn die Aufgabe nicht mehr diesem Worker gehört.
pub async fn heartbeat(
    pool: &PgPool,
    task_id: i64,
    worker: &str,
    cfg: &QueueConfig,
) -> Result<bool, IngestError> {
    let n = sqlx::query(
        "UPDATE index_task SET lease_until = now() + make_interval(secs => $3), updated_at = now() \
         WHERE id = $1 AND worker = $2 AND status = 'running'",
    )
    .bind(task_id)
    .bind(worker)
    .bind(cfg.lease.as_secs_f64())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n > 0)
}

pub async fn complete(pool: &PgPool, task_id: i64) -> Result<(), IngestError> {
    sqlx::query("UPDATE index_task SET status = 'done', lease_until = NULL, error = NULL, updated_at = now() WHERE id = $1")
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Meldet einen Fehler: erneuter Versuch nach Backoff, bei erschöpften Versuchen `failed`.
/// Gibt `true` zurück, wenn die Aufgabe endgültig gescheitert ist.
pub async fn fail(
    pool: &PgPool,
    task: &Task,
    error: &str,
    cfg: &QueueConfig,
) -> Result<bool, IngestError> {
    let permanent = task.attempts >= cfg.max_attempts;
    let backoff = cfg.retry_backoff.as_secs_f64() * f64::from(task.attempts.max(1));
    sqlx::query(
        "UPDATE index_task SET status = CASE WHEN $4 THEN 'failed' ELSE 'pending' END, \
         error = $2, lease_until = NULL, updated_at = now(), \
         not_before = now() + make_interval(secs => $3) WHERE id = $1",
    )
    .bind(task.id)
    .bind(error)
    .bind(backoff)
    .bind(permanent)
    .execute(pool)
    .await?;
    Ok(permanent)
}

// -------------------------------------------------------------- Dokumente

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentInfo {
    pub id: Uuid,
    pub rel_path: String,
    pub kind: DocKind,
}

pub async fn document_info(pool: &PgPool, id: Uuid) -> Result<Option<DocumentInfo>, IngestError> {
    let row = sqlx::query("SELECT id, rel_path, kind FROM document WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(|r| {
        let kind: String = r.try_get("kind")?;
        Ok(DocumentInfo {
            id: r.try_get("id")?,
            rel_path: r.try_get("rel_path")?,
            kind: DocKind::parse(&kind)
                .ok_or_else(|| IngestError::Other(format!("unbekannte Dokumentart {kind}")))?,
        })
    })
    .transpose()
}

pub async fn set_extracting(pool: &PgPool, id: Uuid) -> Result<(), IngestError> {
    sqlx::query(
        "UPDATE document SET status = 'extracting', error = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_pages(pool: &PgPool, id: Uuid, pages: u32) -> Result<(), IngestError> {
    sqlx::query("UPDATE document SET pages = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(pages as i32)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_failed(pool: &PgPool, id: Uuid, error: &str) -> Result<(), IngestError> {
    sqlx::query(
        "UPDATE document SET status = 'failed', error = $2, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Hängt eine Fehlernotiz an (z. B. eine gescheiterte OCR-Seite), ohne den Status zu ändern.
pub async fn note_error(pool: &PgPool, id: Uuid, error: &str) -> Result<(), IngestError> {
    sqlx::query("UPDATE document SET error = left(coalesce(error || E'\\n', '') || $2, 2000), updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(error)
        .execute(pool)
        .await?;
    Ok(())
}

/// Markiert das Dokument als `indexed`, sobald keine offene Aufgabe mehr existiert.
pub async fn finalize_if_idle(pool: &PgPool, id: Uuid) -> Result<bool, IngestError> {
    let n = sqlx::query(
        "UPDATE document SET status = 'indexed', indexed_at = now(), updated_at = now() \
         WHERE id = $1 AND status = 'extracting' AND NOT EXISTS ( \
            SELECT 1 FROM index_task WHERE document_id = $1 AND status IN ('pending', 'running'))",
    )
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Löscht Chunks (und mit `all`: alle Seiten) eines Dokuments. Mit `page` nur diese Seite.
pub async fn delete_chunks(pool: &PgPool, id: Uuid, page: Option<u32>) -> Result<(), IngestError> {
    match page {
        Some(p) => {
            sqlx::query("DELETE FROM chunk WHERE document_id = $1 AND page = $2")
                .bind(id)
                .bind(p as i32)
                .execute(pool)
                .await?
        }
        None => {
            sqlx::query("DELETE FROM chunk WHERE document_id = $1")
                .bind(id)
                .execute(pool)
                .await?
        }
    };
    Ok(())
}

/// Löscht noch offene Folgeaufgaben (`ocr_page`, `embed`) – beim Neu-Extrahieren.
pub async fn delete_followup_tasks(pool: &PgPool, id: Uuid) -> Result<(), IngestError> {
    sqlx::query("DELETE FROM index_task WHERE document_id = $1 AND kind <> 'extract'")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Schreibt Chunks eines Dokuments. `ord` = Seite × 1000 + laufende Nummer (ohne Seite: laufende Nummer),
/// damit Seiten unabhängig voneinander (neu) geschrieben werden können und die Reihenfolge stimmt.
/// Gibt die Zahl der geschriebenen Chunks zurück.
pub async fn insert_chunks(
    pool: &PgPool,
    document_id: Uuid,
    chunks: &[Chunk],
) -> Result<usize, IngestError> {
    let mut tx = pool.begin().await?;
    let mut counters: std::collections::HashMap<Option<u32>, i32> =
        std::collections::HashMap::new();
    for chunk in chunks {
        let n = counters.entry(chunk.page).or_insert(0);
        let ord = chunk.page.map_or(0, |p| p as i32 * 1000) + *n;
        *n += 1;
        sqlx::query("INSERT INTO chunk (document_id, page, ord, text) VALUES ($1, $2, $3, $4)")
            .bind(document_id)
            .bind(chunk.page.map(|p| p as i32))
            .bind(ord)
            .bind(&chunk.text)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(chunks.len())
}

// ------------------------------------------------------------- Synchronisation

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub scanned: usize,
    pub new: usize,
    pub changed: usize,
    pub unchanged_touched: usize,
    pub deleted: usize,
    pub errors: usize,
}

async fn known_documents(pool: &PgPool) -> Result<Vec<KnownDoc>, IngestError> {
    let rows = sqlx::query("SELECT rel_path, size, mtime FROM document")
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|r| {
            Ok(KnownDoc {
                rel_path: r.try_get("rel_path")?,
                size: r.try_get("size")?,
                mtime: r.try_get::<DateTime<Utc>, _>("mtime")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

async fn hash(path: &Path) -> Result<String, IngestError> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        scan::sha256_file(&p).map_err(|source| IngestError::Io { path: p, source })
    })
    .await
    .map_err(|e| IngestError::Other(e.to_string()))?
}

/// Scannt `root`, gleicht mit `document` ab und reiht `extract`-Tasks ein:
/// - neu: Zeile + `extract`
/// - geändert (Hash anders): Chunks und Folgeaufgaben weg, Zeile zurücksetzen, `extract`
/// - nur Zeitstempel anders (Hash gleich): Zeile aktualisieren
/// - gelöscht: Zeile samt Chunks und Aufgaben löschen (Kaskade)
///
/// Ist der Scan leer, die Datenbank aber nicht, wird abgebrochen (Laufwerk nicht eingebunden?),
/// statt den ganzen Index zu löschen.
pub async fn sync(pool: &PgPool, root: &Path) -> Result<SyncReport, IngestError> {
    let root_buf = root.to_path_buf();
    let scanned = tokio::task::spawn_blocking(move || scan::scan_dir(&root_buf))
        .await
        .map_err(|e| IngestError::Other(e.to_string()))??;
    let known = known_documents(pool).await?;
    if scanned.is_empty() && !known.is_empty() {
        return Err(IngestError::EmptyArchive(root.to_path_buf()));
    }

    let mut report = SyncReport {
        scanned: scanned.len(),
        ..Default::default()
    };
    let diff = scan::diff(scanned, &known);

    for file in &diff.new {
        match insert_new(pool, file).await {
            Ok(()) => report.new += 1,
            Err(err) => {
                tracing::warn!(path = %file.rel_path, %err, "Neues Dokument konnte nicht angelegt werden");
                report.errors += 1;
            }
        }
    }
    for file in &diff.changed {
        match update_changed(pool, file).await {
            Ok(true) => report.changed += 1,
            Ok(false) => report.unchanged_touched += 1,
            Err(err) => {
                tracing::warn!(path = %file.rel_path, %err, "Geändertes Dokument konnte nicht aktualisiert werden");
                report.errors += 1;
            }
        }
    }
    for rel_path in &diff.deleted {
        sqlx::query("DELETE FROM document WHERE rel_path = $1")
            .bind(rel_path)
            .execute(pool)
            .await?;
        report.deleted += 1;
    }
    Ok(report)
}

async fn insert_new(pool: &PgPool, file: &ScannedFile) -> Result<(), IngestError> {
    let sha = hash(&file.abs_path).await?;
    let id: Uuid = sqlx::query(
        "INSERT INTO document (rel_path, size, mtime, sha256, kind) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (rel_path) DO NOTHING RETURNING id",
    )
    .bind(&file.rel_path)
    .bind(file.size as i64)
    .bind(file.mtime)
    .bind(&sha)
    .bind(file.kind.as_str())
    .fetch_optional(pool)
    .await?
    .map(|r| r.try_get("id"))
    .transpose()?
    .ok_or_else(|| IngestError::Other("Dokument existiert bereits".into()))?;
    enqueue(pool, id, TaskKind::Extract, None, 0).await
}

/// `true`, wenn der Inhalt neu extrahiert wird.
async fn update_changed(pool: &PgPool, file: &ScannedFile) -> Result<bool, IngestError> {
    let sha = hash(&file.abs_path).await?;
    let row = sqlx::query("SELECT id, sha256 FROM document WHERE rel_path = $1")
        .bind(&file.rel_path)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else { return Ok(false) };
    let id: Uuid = row.try_get("id")?;
    let old: String = row.try_get("sha256")?;

    if old == sha {
        sqlx::query("UPDATE document SET size = $2, mtime = $3, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(file.size as i64)
            .bind(file.mtime)
            .execute(pool)
            .await?;
        return Ok(false);
    }
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM chunk WHERE document_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM index_task WHERE document_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE document SET size = $2, mtime = $3, sha256 = $4, kind = $5, status = 'new', pages = NULL, \
         error = NULL, indexed_at = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(file.size as i64)
    .bind(file.mtime)
    .bind(&sha)
    .bind(file.kind.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO index_task (document_id, kind) VALUES ($1, 'extract')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}
