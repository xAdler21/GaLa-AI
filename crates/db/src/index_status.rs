//! Zustand des Dokumenten-Index für das Admin-Panel: Lebenszeichen der Worker
//! (Tabelle `worker_status`) und Zähler über Warteschlange und Dokumente.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// Ein Lebenszeichen, wie es der Worker schreibt.
pub struct WorkerBeat<'a> {
    pub name: &'a str,
    /// `cpu` oder `vision`.
    pub role: &'a str,
    /// Nur Rolle vision: `cpu` oder `cuda:N`.
    pub ocr_device: Option<&'a str>,
    pub build_rev: Option<&'a str>,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkerStatus {
    pub name: String,
    pub role: String,
    pub ocr_device: Option<String>,
    pub build_rev: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// Schreibt bzw. erneuert die Zeile dieses Workers.
pub async fn worker_heartbeat(pool: &PgPool, beat: &WorkerBeat<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO worker_status (name, role, ocr_device, build_rev, started_at, last_seen) \
         VALUES ($1, $2, $3, $4, $5, now()) \
         ON CONFLICT (name) DO UPDATE SET role = EXCLUDED.role, ocr_device = EXCLUDED.ocr_device, \
         build_rev = EXCLUDED.build_rev, started_at = EXCLUDED.started_at, last_seen = now()",
    )
    .bind(beat.name)
    .bind(beat.role)
    .bind(beat.ocr_device)
    .bind(beat.build_rev)
    .bind(beat.started_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Entfernt Zeilen von Workern, die sich seit `days` Tagen nicht gemeldet haben
/// (alte Namen, z. B. Container-IDs früherer Container).
pub async fn prune_workers(pool: &PgPool, days: i32) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(
        "DELETE FROM worker_status WHERE last_seen < now() - make_interval(days => $1)",
    )
    .bind(days)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Alle bekannten Worker, zuletzt gemeldete zuerst.
pub async fn workers(pool: &PgPool) -> Result<Vec<WorkerStatus>, sqlx::Error> {
    sqlx::query_as::<_, WorkerStatus>(
        "SELECT name, role, ocr_device, build_rev, started_at, last_seen \
         FROM worker_status ORDER BY last_seen DESC, name",
    )
    .fetch_all(pool)
    .await
}

/// Anzahl der Aufgaben je Art und Zustand.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskCount {
    /// `extract`, `ocr_page` oder `embed`.
    pub kind: String,
    /// `pending`, `running` oder `failed`.
    pub status: String,
    pub count: i64,
}

/// Offene und endgültig gescheiterte Aufgaben. Erledigte werden nicht gezählt: das wären
/// bei einem großen Archiv Hunderttausende Zeilen pro Abfrage.
pub async fn open_task_counts(pool: &PgPool) -> Result<Vec<TaskCount>, sqlx::Error> {
    sqlx::query_as::<_, TaskCount>(
        "SELECT kind, status, count(*) AS count FROM index_task \
         WHERE status IN ('pending', 'running', 'failed') GROUP BY kind, status",
    )
    .fetch_all(pool)
    .await
}

/// Anzahl der Dokumente je Zustand (`new`, `extracting`, `indexed`, `failed`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DocumentCount {
    pub status: String,
    pub count: i64,
}

pub async fn document_counts(pool: &PgPool) -> Result<Vec<DocumentCount>, sqlx::Error> {
    sqlx::query_as::<_, DocumentCount>(
        "SELECT status, count(*) AS count FROM document GROUP BY status",
    )
    .fetch_all(pool)
    .await
}
