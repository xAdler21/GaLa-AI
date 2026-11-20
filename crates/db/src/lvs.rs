//! Leistungsverzeichnisse (Tabelle `lv`). Das LV selbst liegt als JSON vor; die Typen
//! (`lv_model::Lv`) kennt erst der Coordinator.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Zeile ohne Originaldatei und LV (für die Liste).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LvRow {
    pub id: Uuid,
    pub name: String,
    pub format: String,
    pub file_name: String,
    pub positions: i32,
    pub priced: i32,
    pub total: String,
    pub revision: i32,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Vollständiger Datensatz.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LvFull {
    #[sqlx(flatten)]
    pub row: LvRow,
    pub original: Vec<u8>,
    pub lv: serde_json::Value,
    pub warnings: serde_json::Value,
}

/// Kennzahlen, die mit dem LV gespeichert werden.
#[derive(Debug, Clone)]
pub struct LvStats {
    pub positions: i32,
    pub priced: i32,
    pub total: String,
}

pub struct NewLv<'a> {
    pub name: &'a str,
    pub format: &'a str,
    pub file_name: &'a str,
    pub original: &'a [u8],
    pub lv: serde_json::Value,
    pub warnings: serde_json::Value,
    pub stats: LvStats,
    pub created_by: Uuid,
}

macro_rules! row_columns {
    () => {
        "l.id, l.name, l.format, l.file_name, l.positions, l.priced, l.total, l.revision, \
         u.username AS created_by, l.created_at, l.updated_at"
    };
}

pub async fn insert(pool: &PgPool, n: NewLv<'_>) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO lv (name, format, file_name, original, lv, warnings, positions, priced, total, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(n.name)
    .bind(n.format)
    .bind(n.file_name)
    .bind(n.original)
    .bind(n.lv)
    .bind(n.warnings)
    .bind(n.stats.positions)
    .bind(n.stats.priced)
    .bind(n.stats.total)
    .bind(n.created_by)
    .fetch_one(pool)
    .await
}

pub async fn list(pool: &PgPool) -> Result<Vec<LvRow>, sqlx::Error> {
    sqlx::query_as::<_, LvRow>(concat!(
        "SELECT ",
        row_columns!(),
        " FROM lv l LEFT JOIN app_user u ON u.id = l.created_by ORDER BY l.updated_at DESC"
    ))
    .fetch_all(pool)
    .await
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<LvFull>, sqlx::Error> {
    sqlx::query_as::<_, LvFull>(concat!(
        "SELECT ",
        row_columns!(),
        ", l.original, l.lv, l.warnings FROM lv l LEFT JOIN app_user u ON u.id = l.created_by WHERE l.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Speichert das LV, wenn niemand seit `revision` gespeichert hat. Gibt die neue Revision
/// zurück, `None` bei Konflikt oder unbekannter ID.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    revision: i32,
    lv: serde_json::Value,
    stats: LvStats,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE lv SET lv = $3, positions = $4, priced = $5, total = $6, \
         revision = revision + 1, updated_at = now() \
         WHERE id = $1 AND revision = $2 RETURNING revision",
    )
    .bind(id)
    .bind(revision)
    .bind(lv)
    .bind(stats.positions)
    .bind(stats.priced)
    .bind(stats.total)
    .fetch_optional(pool)
    .await
}

pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("DELETE FROM lv WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}
