//! Nutzerverwaltung (Tabelle `app_user`).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub pw_hash: String,
    /// `admin` oder `kalkulator`.
    pub role: String,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
}

macro_rules! columns {
    () => {
        "id, username, pw_hash, role, disabled, created_at"
    };
}

pub async fn create(
    pool: &PgPool,
    username: &str,
    pw_hash: &str,
    role: &str,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(concat!(
        "INSERT INTO app_user (username, pw_hash, role) VALUES ($1, $2, $3) RETURNING ",
        columns!(),
        ""
    ))
    .bind(username)
    .bind(pw_hash)
    .bind(role)
    .fetch_one(pool)
    .await
}

pub async fn find_by_username(pool: &PgPool, username: &str) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        columns!(),
        " FROM app_user WHERE username = $1"
    ))
    .bind(username)
    .fetch_optional(pool)
    .await
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        columns!(),
        " FROM app_user WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list(pool: &PgPool) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(concat!(
        "SELECT ",
        columns!(),
        " FROM app_user ORDER BY username"
    ))
    .fetch_all(pool)
    .await
}

/// Sperrt bzw. entsperrt einen Nutzer. Beim Sperren werden alle Sitzungen gelöscht.
pub async fn set_disabled(pool: &PgPool, id: Uuid, disabled: bool) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("UPDATE app_user SET disabled = $2 WHERE id = $1")
        .bind(id)
        .bind(disabled)
        .execute(pool)
        .await?;
    if disabled {
        sqlx::query("DELETE FROM session WHERE user_id = $1")
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(res.rows_affected() > 0)
}

/// Setzt ein neues Passwort (Hash) und beendet alle Sitzungen des Nutzers.
pub async fn set_password_by_id(
    pool: &PgPool,
    id: Uuid,
    pw_hash: &str,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("UPDATE app_user SET pw_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(pw_hash)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM session WHERE user_id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn set_password_by_username(
    pool: &PgPool,
    username: &str,
    pw_hash: &str,
) -> Result<bool, sqlx::Error> {
    match find_by_username(pool, username).await? {
        Some(u) => set_password_by_id(pool, u.id, pw_hash).await,
        None => Ok(false),
    }
}

/// Anzahl aktiver Admins (für „letzten Admin nicht sperren“).
pub async fn count_active_admins(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM app_user WHERE role = 'admin' AND NOT disabled")
        .fetch_one(pool)
        .await
}
