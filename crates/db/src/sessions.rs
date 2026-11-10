//! Sitzungen (Tabelle `session`). Gespeichert wird nur der SHA-256-Hash des Tokens.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Gültige Sitzung samt Nutzerdaten.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionUser {
    pub user_id: Uuid,
    pub username: String,
    pub role: String,
    pub kind: String,
    pub expires_at: DateTime<Utc>,
}

pub async fn create(
    pool: &PgPool,
    token_hash: &[u8],
    user_id: Uuid,
    kind: &str,
    device_name: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO session (token_hash, user_id, kind, device_name, expires_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(token_hash)
        .bind(user_id)
        .bind(kind)
        .bind(device_name)
        .bind(expires_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Sucht eine nicht abgelaufene Sitzung eines nicht gesperrten Nutzers und aktualisiert `last_seen`.
pub async fn find_valid(
    pool: &PgPool,
    token_hash: &[u8],
    kind: &str,
) -> Result<Option<SessionUser>, sqlx::Error> {
    let found = sqlx::query_as::<_, SessionUser>(
        "SELECT s.user_id, u.username, u.role, s.kind, s.expires_at \
         FROM session s JOIN app_user u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.kind = $2 AND s.expires_at > now() AND NOT u.disabled",
    )
    .bind(token_hash)
    .bind(kind)
    .fetch_optional(pool)
    .await?;
    if found.is_some() {
        sqlx::query("UPDATE session SET last_seen = now() WHERE token_hash = $1 AND last_seen < now() - interval '1 minute'")
            .bind(token_hash)
            .execute(pool)
            .await?;
    }
    Ok(found)
}

pub async fn delete(pool: &PgPool, token_hash: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM session WHERE token_hash = $1")
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Räumt abgelaufene Sitzungen auf; liefert die Anzahl.
pub async fn delete_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM session WHERE expires_at <= now()")
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
