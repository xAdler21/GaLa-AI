//! Audit-Log (Tabelle `audit_log`).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub user_id: Option<Uuid>,
    /// Anzeigename des Nutzers (per Join, falls vorhanden).
    pub username: Option<String>,
    pub action: String,
    pub detail: serde_json::Value,
}

/// Schreibt einen Eintrag. Fehler werden nur protokolliert, damit ein Audit-Problem
/// die eigentliche Aktion nicht verhindert.
pub async fn record(pool: &PgPool, user_id: Option<Uuid>, action: &str, detail: serde_json::Value) {
    let res = sqlx::query("INSERT INTO audit_log (user_id, action, detail) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(action)
        .bind(detail)
        .execute(pool)
        .await;
    if let Err(e) = res {
        tracing::error!(action, error = %e, "Audit-Eintrag fehlgeschlagen");
    }
}

pub async fn recent(pool: &PgPool, limit: i64) -> Result<Vec<AuditEntry>, sqlx::Error> {
    sqlx::query_as::<_, AuditEntry>(
        "SELECT a.id, a.at, a.user_id, u.username, a.action, a.detail \
         FROM audit_log a LEFT JOIN app_user u ON u.id = a.user_id \
         ORDER BY a.at DESC, a.id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}
