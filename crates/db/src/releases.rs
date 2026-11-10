//! Desktop-Releases (Tabelle `release`).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Release {
    pub id: Uuid,
    pub version: String,
    pub platform: String,
    pub file_name: String,
    pub signature: String,
    pub notes: String,
    pub published: bool,
    pub created_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
}

macro_rules! columns {
    () => {
        "id, version, platform, file_name, signature, notes, published, created_at, published_at, created_by"
    };
}

pub struct NewRelease<'a> {
    pub version: &'a str,
    pub platform: &'a str,
    pub file_name: &'a str,
    pub signature: &'a str,
    pub notes: &'a str,
    pub created_by: Option<&'a str>,
}

pub async fn insert(pool: &PgPool, r: &NewRelease<'_>) -> Result<Release, sqlx::Error> {
    sqlx::query_as::<_, Release>(concat!(
        "INSERT INTO release (version, platform, file_name, signature, notes, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING ",
        columns!(),
        ""
    ))
    .bind(r.version)
    .bind(r.platform)
    .bind(r.file_name)
    .bind(r.signature)
    .bind(r.notes)
    .bind(r.created_by)
    .fetch_one(pool)
    .await
}

pub async fn list(pool: &PgPool) -> Result<Vec<Release>, sqlx::Error> {
    sqlx::query_as::<_, Release>(concat!(
        "SELECT ",
        columns!(),
        " FROM release ORDER BY created_at DESC"
    ))
    .fetch_all(pool)
    .await
}

/// Alle veröffentlichten Releases einer Plattform (Sortierung nach SemVer macht der Aufrufer).
pub async fn list_published(pool: &PgPool, platform: &str) -> Result<Vec<Release>, sqlx::Error> {
    sqlx::query_as::<_, Release>(concat!(
        "SELECT ",
        columns!(),
        " FROM release WHERE published AND platform = $1"
    ))
    .bind(platform)
    .fetch_all(pool)
    .await
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<Release>, sqlx::Error> {
    sqlx::query_as::<_, Release>(concat!(
        "SELECT ",
        columns!(),
        " FROM release WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn get_by_version(pool: &PgPool, version: &str) -> Result<Option<Release>, sqlx::Error> {
    sqlx::query_as::<_, Release>(concat!(
        "SELECT ",
        columns!(),
        " FROM release WHERE version = $1"
    ))
    .bind(version)
    .fetch_optional(pool)
    .await
}

pub async fn set_published(pool: &PgPool, id: Uuid, published: bool) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE release SET published = $2, \
         published_at = CASE WHEN $2 THEN now() ELSE NULL END WHERE id = $1",
    )
    .bind(id)
    .bind(published)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("DELETE FROM release WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}
