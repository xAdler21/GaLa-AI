//! Wartungsmodus: Middleware für `/api/v1` und Hilfsfunktionen zum Setzen.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use db::settings::Maintenance;

use crate::error::ApiErr;
use crate::state::AppState;

/// Routen, die auch im Wartungsmodus antworten: `health`, `auth/*`, `updates/*`.
pub fn is_exempt(path: &str) -> bool {
    path == "/api/v1/health"
        || path.starts_with("/api/v1/auth/")
        || path.starts_with("/api/v1/updates/")
}

/// Standardtext, wenn keine Nachricht gesetzt ist.
pub const DEFAULT_MESSAGE: &str = "Der Server befindet sich im Wartungsmodus.";

pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if is_exempt(req.uri().path()) {
        return next.run(req).await;
    }
    match db::settings::get_maintenance(&state.db).await {
        Ok(m) if m.enabled => ApiErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "maintenance",
            m.message
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_MESSAGE.into()),
        )
        .into_response(),
        Ok(_) => next.run(req).await,
        Err(e) => ApiErr::from(e).into_response(),
    }
}

/// Schaltet den Wartungsmodus und schreibt einen Audit-Eintrag.
pub async fn set(
    state: &AppState,
    enabled: bool,
    message: Option<String>,
    by: Option<(uuid::Uuid, &str)>,
    source: &str,
) -> Result<Maintenance, db::SqlxError> {
    let message = message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let m = if enabled {
        Maintenance {
            enabled: true,
            message,
            since: Some(Utc::now()),
        }
    } else {
        Maintenance::default()
    };
    db::settings::set_maintenance(&state.db, &m, by.map(|b| b.1)).await?;
    db::audit::record(
        &state.db,
        by.map(|b| b.0),
        "maintenance.set",
        serde_json::json!({ "enabled": m.enabled, "message": m.message, "source": source }),
    )
    .await;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert!(is_exempt("/api/v1/health"));
        assert!(is_exempt("/api/v1/auth/login"));
        assert!(is_exempt("/api/v1/auth/logout"));
        assert!(is_exempt("/api/v1/updates/windows/x86_64/0.1.0"));
        assert!(!is_exempt("/api/v1/chat"));
        assert!(!is_exempt("/api/v1/me"));
        assert!(!is_exempt("/api/v1/healthz"));
        assert!(!is_exempt("/api/v1/authx"));
        assert!(!is_exempt("/api/v1/updates"));
    }
}
