//! Desktop-API unter `/api/v1` (DTOs: `api_types::client`).

use api_types::client::{
    ChatRequest, ChatResponse, HealthResponse, LoginRequest, LoginResponse, MaintenanceInfo, Role,
    UserInfo,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use chrono::{Duration, Utc};
use db::sessions::SessionUser;

use crate::auth::{self, ClientIp};
use crate::error::ApiErr;
use crate::state::AppState;
use crate::{chatbot, maintenance, updates};

/// Gültigkeit eines Desktop-Tokens.
const TOKEN_DAYS: i64 = 30;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/chat", post(chat))
        .route(
            "/api/v1/updates/{target}/{arch}/{current_version}",
            get(update_check),
        )
        .layer(middleware::from_fn_with_state(
            state,
            maintenance::middleware,
        ))
}

/// Parst die Rolle aus der DB; unbekannte Werte sind die geringste Rolle.
pub fn parse_role(s: &str) -> Role {
    match s {
        "admin" => Role::Admin,
        _ => Role::Kalkulator,
    }
}

/// Angemeldeter Desktop-Nutzer (Bearer-Token).
pub struct AuthUser(pub SessionUser);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiErr;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiErr> {
        let token = auth::bearer_token(&parts.headers)
            .ok_or_else(|| ApiErr::unauthorized("Anmeldung erforderlich"))?;
        let session = db::sessions::find_valid(&state.db, &auth::hash_token(token), "desktop")
            .await?
            .ok_or_else(|| ApiErr::unauthorized("Sitzung ungültig oder abgelaufen"))?;
        Ok(Self(session))
    }
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiErr> {
    let m = db::settings::get_maintenance(&state.db).await?;
    let min_client_version = db::settings::get_raw(&state.db, "min_client_version")
        .await?
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.is_empty());
    Ok(Json(HealthResponse {
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        maintenance: MaintenanceInfo {
            enabled: m.enabled,
            message: m.message,
            since: m.since,
        },
        min_client_version,
    }))
}

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    payload: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Json<LoginResponse>, ApiErr> {
    let Json(req) = payload.map_err(|e| ApiErr::bad_request(format!("Ungültige Anfrage: {e}")))?;
    let username = req.username.trim().to_string();
    if username.is_empty() || req.password.is_empty() {
        return Err(ApiErr::bad_request(
            "Benutzername und Passwort erforderlich",
        ));
    }
    if let Some(wait) = state.limiter.blocked(&username, &ip) {
        return Err(ApiErr::too_many(format!(
            "Zu viele Fehlversuche, bitte in {} Minuten erneut versuchen",
            wait.as_secs().div_ceil(60).max(1)
        )));
    }

    let user = db::users::find_by_username(&state.db, &username).await?;
    // Argon2 ist rechenintensiv: nicht auf dem Async-Executor ausführen. Auch bei unbekanntem
    // Nutzer wird gerechnet, damit die Antwortzeit nichts verrät.
    let hash = user
        .as_ref()
        .map_or_else(|| auth::dummy_hash().to_string(), |u| u.pw_hash.clone());
    let password = req.password;
    let password_ok = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash))
        .await
        .unwrap_or(false);

    let user = match user {
        Some(u) if password_ok && !u.disabled => u,
        _ => {
            state.limiter.record_failure(&username, &ip);
            db::audit::record(
                &state.db,
                None,
                "login.failed",
                serde_json::json!({ "username": username, "ip": ip, "kind": "desktop" }),
            )
            .await;
            return Err(ApiErr::unauthorized("Benutzername oder Passwort falsch"));
        }
    };
    state.limiter.clear(&username, &ip);

    let token = auth::new_token();
    let expires_at = Utc::now() + Duration::days(TOKEN_DAYS);
    let device = req
        .device_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    db::sessions::create(
        &state.db,
        &auth::hash_token(&token),
        user.id,
        "desktop",
        device,
        expires_at,
    )
    .await?;
    db::audit::record(
        &state.db,
        Some(user.id),
        "login",
        serde_json::json!({ "kind": "desktop", "device": device, "ip": ip }),
    )
    .await;

    Ok(Json(LoginResponse {
        token,
        expires_at,
        user: UserInfo {
            id: user.id,
            username: user.username,
            role: parse_role(&user.role),
        },
    }))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<StatusCode, ApiErr> {
    if let Some(token) = auth::bearer_token(&headers) {
        db::sessions::delete(&state.db, &auth::hash_token(token)).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn me(AuthUser(s): AuthUser) -> Json<UserInfo> {
    Json(UserInfo {
        id: s.user_id,
        username: s.username,
        role: parse_role(&s.role),
    })
}

/// Dokumenten-Chatbot. Ohne Embedder (Feature `embed`) 501, bei nicht erreichbarem LLM 503.
async fn chat(
    State(state): State<AppState>,
    _user: AuthUser,
    payload: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Json<ChatResponse>, ApiErr> {
    let Json(req) = payload.map_err(|e| ApiErr::bad_request(format!("Ungültige Anfrage: {e}")))?;
    chatbot::run(state.chat.as_deref(), &req).await.map(Json)
}

async fn update_check(
    State(state): State<AppState>,
    Path((target, arch, current_version)): Path<(String, String, String)>,
) -> Result<Response, ApiErr> {
    let current = updates::parse_version(&current_version)
        .ok_or_else(|| ApiErr::bad_request("Ungültige Versionsnummer"))?;
    let platform = format!("{target}-{arch}");
    let releases = db::releases::list_published(&state.db, &platform).await?;
    Ok(
        match updates::decide(&releases, &current, &state.cfg.public_url) {
            Some(update) => Json(update).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        },
    )
}
