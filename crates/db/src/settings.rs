//! Einstellungen (Tabelle `setting`, Key → jsonb) mit typisierten Zugriffen.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::PgPool;

pub const KEY_MAINTENANCE: &str = "maintenance";
pub const KEY_CLAUDE_ACCESS: &str = "claude_access";

/// `{"enabled": bool, "message": string|null, "since": rfc3339|null}`
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Maintenance {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
}

/// `{"enabled": bool, "changed_at": rfc3339|null, "changed_by": string|null}`
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClaudeAccess {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub changed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub changed_by: Option<String>,
}

pub async fn get_raw(pool: &PgPool, key: &str) -> Result<Option<serde_json::Value>, sqlx::Error> {
    sqlx::query_scalar::<_, serde_json::Value>("SELECT value FROM setting WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

pub async fn set_raw(
    pool: &PgPool,
    key: &str,
    value: &serde_json::Value,
    updated_by: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO setting (key, value, updated_at, updated_by) VALUES ($1, $2, now(), $3) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by",
    )
    .bind(key)
    .bind(value)
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Liest einen Wert; fehlt er oder ist er unlesbar, kommt der Standardwert.
pub async fn get_or_default<T: DeserializeOwned + Default>(
    pool: &PgPool,
    key: &str,
) -> Result<T, sqlx::Error> {
    Ok(match get_raw(pool, key).await? {
        Some(v) => serde_json::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(key, error = %e, "Einstellung unlesbar, Standardwert");
            T::default()
        }),
        None => T::default(),
    })
}

pub async fn get_maintenance(pool: &PgPool) -> Result<Maintenance, sqlx::Error> {
    get_or_default(pool, KEY_MAINTENANCE).await
}

pub async fn set_maintenance(
    pool: &PgPool,
    m: &Maintenance,
    by: Option<&str>,
) -> Result<(), sqlx::Error> {
    set_raw(
        pool,
        KEY_MAINTENANCE,
        &serde_json::to_value(m).unwrap_or_default(),
        by,
    )
    .await
}

pub async fn get_claude_access(pool: &PgPool) -> Result<ClaudeAccess, sqlx::Error> {
    get_or_default(pool, KEY_CLAUDE_ACCESS).await
}

pub async fn set_claude_access(
    pool: &PgPool,
    c: &ClaudeAccess,
    by: Option<&str>,
) -> Result<(), sqlx::Error> {
    set_raw(
        pool,
        KEY_CLAUDE_ACCESS,
        &serde_json::to_value(c).unwrap_or_default(),
        by,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off() {
        assert!(!Maintenance::default().enabled);
        assert!(!ClaudeAccess::default().enabled);
    }

    #[test]
    fn partial_json_is_accepted() {
        let m: Maintenance = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(m.enabled && m.message.is_none());
    }
}
