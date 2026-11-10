//! Interne API (nur auf dem internen Listener): Claude-Zugang abfragen, Wartungsmodus setzen.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::error::ApiErr;
use crate::maintenance;
use crate::state::AppState;

pub const TOKEN_HEADER: &str = "x-gala-internal-token";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/internal/claude-access", get(claude_access))
        .route("/internal/maintenance", post(set_maintenance))
}

/// Konstantzeitvergleich; ohne konfiguriertes Token wird alles abgelehnt.
pub fn token_ok(expected: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(expected) = expected.filter(|e| !e.is_empty()) else {
        return false;
    };
    let Some(given) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    given.as_bytes().ct_eq(expected.as_bytes()).into()
}

async fn claude_access(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiErr> {
    let c = db::settings::get_claude_access(&state.db).await?;
    Ok(Json(
        serde_json::json!({ "enabled": c.enabled, "changed_at": c.changed_at }),
    ))
}

#[derive(Debug, Deserialize)]
struct MaintenanceBody {
    enabled: bool,
    #[serde(default)]
    message: Option<String>,
}

async fn set_maintenance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MaintenanceBody>,
) -> Result<StatusCode, ApiErr> {
    if !token_ok(state.cfg.internal_token.as_deref(), &headers) {
        return Err(ApiErr::unauthorized("Interner Token fehlt oder ist falsch"));
    }
    maintenance::set(&state, body.enabled, body.message, None, "internal").await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn h(v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(TOKEN_HEADER, HeaderValue::from_str(v).unwrap());
        m
    }

    #[test]
    fn token_comparison() {
        assert!(token_ok(Some("abc"), &h("abc")));
        assert!(!token_ok(Some("abc"), &h("abd")));
        assert!(!token_ok(Some("abc"), &h("abcd")));
        assert!(!token_ok(Some("abc"), &HeaderMap::new()));
    }

    #[test]
    fn unset_token_rejects_everything() {
        assert!(!token_ok(None, &h("")));
        assert!(!token_ok(Some(""), &h("")));
        assert!(!token_ok(None, &h("abc")));
    }
}
