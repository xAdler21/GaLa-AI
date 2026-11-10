//! HTTP zum Coordinator. Alle Aufrufe laufen hier (reqwest mit Schannel, also dem
//! Windows-Zertifikatsspeicher), damit die Oberflaeche kein CORS und keine eigene TLS-Konfiguration braucht.

use std::error::Error as _;
use std::time::Duration;

use api_types::client::{ApiError, HealthResponse};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub const STORE_FILE: &str = "settings.json";
const KEY_SERVER: &str = "server_url";
const KEY_TOKEN: &str = "token";

pub type CmdResult<T> = Result<T, ApiError>;

pub fn err(code: &str, message: impl Into<String>) -> ApiError {
    ApiError { code: code.to_owned(), message: message.into() }
}

pub fn build_client() -> Client {
    Client::builder()
        .user_agent(concat!("GaLa-AI-Desktop/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(180))
        .build()
        .expect("HTTP-Client konnte nicht erstellt werden")
}

/// Macht aus Nutzereingaben wie `gala-coordinator` eine Basis-URL ohne abschliessenden Slash.
pub fn normalize_url(raw: &str) -> CmdResult<String> {
    let mut s = raw.trim().trim_end_matches('/').to_owned();
    if s.is_empty() {
        return Err(err("invalid_url", "Bitte eine Server-Adresse eingeben."));
    }
    if !s.contains("://") {
        s = format!("https://{s}");
    }
    let url = reqwest::Url::parse(&s)
        .map_err(|_| err("invalid_url", "Die Server-Adresse ist ungueltig."))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(err("invalid_url", "Die Server-Adresse muss mit https:// beginnen."));
    }
    Ok(s)
}

/// Fehlerkette eines reqwest-Fehlers als lesbarer Text (zeigt z. B. TLS-Probleme).
fn net_err(e: reqwest::Error) -> ApiError {
    let mut msg = if e.is_timeout() {
        "Zeitueberschreitung bei der Verbindung zum Server.".to_owned()
    } else if e.is_connect() {
        "Der Server ist nicht erreichbar.".to_owned()
    } else {
        "Netzwerkfehler.".to_owned()
    };
    let mut source = e.source();
    let mut details = Vec::new();
    while let Some(s) = source {
        details.push(s.to_string());
        source = s.source();
    }
    if !details.is_empty() {
        msg.push_str(" (");
        msg.push_str(&details.join(": "));
        msg.push(')');
    }
    err("network", msg)
}

async fn parse<T: DeserializeOwned>(resp: Response) -> CmdResult<T> {
    let status = resp.status();
    if status.is_success() {
        return resp
            .json::<T>()
            .await
            .map_err(|e| err("bad_response", format!("Unerwartete Antwort vom Server: {e}")));
    }
    let body = resp.text().await.unwrap_or_default();
    let mut e = serde_json::from_str::<ApiError>(&body).unwrap_or_else(|_| {
        err(&format!("http_{}", status.as_u16()), format!("Unerwartete Antwort vom Server (HTTP {status})."))
    });
    match status {
        StatusCode::UNAUTHORIZED => e.code = "unauthorized".to_owned(),
        StatusCode::NOT_IMPLEMENTED => e.code = "not_implemented".to_owned(),
        _ => {}
    }
    Err(e)
}

async fn send<T: DeserializeOwned>(req: RequestBuilder) -> CmdResult<T> {
    parse(req.send().await.map_err(net_err)?).await
}

// --- Persistenz ---------------------------------------------------------------------------

pub fn stored_server(app: &AppHandle) -> Option<String> {
    let store = app.store(STORE_FILE).ok()?;
    store.get(KEY_SERVER)?.as_str().map(str::to_owned)
}

pub fn stored_token(app: &AppHandle) -> Option<String> {
    let store = app.store(STORE_FILE).ok()?;
    store.get(KEY_TOKEN)?.as_str().map(str::to_owned)
}

pub fn save_server(app: &AppHandle, url: &str) -> CmdResult<()> {
    let store = app.store(STORE_FILE).map_err(|e| err("store", e.to_string()))?;
    store.set(KEY_SERVER, url);
    store.save().map_err(|e| err("store", e.to_string()))
}

pub fn save_token(app: &AppHandle, token: Option<&str>) -> CmdResult<()> {
    let store = app.store(STORE_FILE).map_err(|e| err("store", e.to_string()))?;
    match token {
        Some(t) => store.set(KEY_TOKEN, t),
        None => {
            store.delete(KEY_TOKEN);
        }
    }
    store.save().map_err(|e| err("store", e.to_string()))
}

pub fn require_server(app: &AppHandle) -> CmdResult<String> {
    stored_server(app).ok_or_else(|| err("no_server", "Es ist noch kein Server eingerichtet."))
}

// --- Aufrufe ------------------------------------------------------------------------------

pub async fn health(client: &Client, base: &str) -> CmdResult<HealthResponse> {
    send(client.get(format!("{base}/api/v1/health")).timeout(Duration::from_secs(10))).await
}

pub async fn post_json_anon<B: serde::Serialize, T: DeserializeOwned>(
    client: &Client,
    base: &str,
    path: &str,
    body: &B,
) -> CmdResult<T> {
    send(client.post(format!("{base}/api/v1{path}")).json(body)).await
}

/// Aufruf mit Bearer-Token. Bei 401 wird der gespeicherte Token verworfen.
pub async fn authed<T: DeserializeOwned>(
    app: &AppHandle,
    client: &Client,
    method: reqwest::Method,
    path: &str,
    body: Option<&impl serde::Serialize>,
) -> CmdResult<T> {
    let base = require_server(app)?;
    let token = stored_token(app).ok_or_else(|| err("unauthorized", "Bitte anmelden."))?;
    let mut req = client.request(method, format!("{base}/api/v1{path}")).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(b);
    }
    let result = send(req).await;
    if let Err(e) = &result
        && e.code == "unauthorized"
    {
        let _ = save_token(app, None);
    }
    result
}

/// Abmelden am Server (Antwort egal), danach Token lokal loeschen.
pub async fn logout_remote(app: &AppHandle, client: &Client) {
    if let (Some(base), Some(token)) = (stored_server(app), stored_token(app)) {
        let _ = client
            .post(format!("{base}/api/v1/auth/logout"))
            .bearer_auth(token)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
    }
}
