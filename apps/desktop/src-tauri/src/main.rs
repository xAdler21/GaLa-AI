//! GaLa-AI Desktop: Tauri-Huelle. Die Oberflaeche (Leptos/WASM) spricht nur ueber die
//! Commands in diesem Crate; HTTP zum Coordinator und der Updater laufen im Rust-Teil.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod net;
mod updates;

use api_types::client::{ChatRequest, ChatResponse, ChatTurn, HealthResponse, LoginRequest, LoginResponse, UserInfo};
use net::{CmdResult, err};
use reqwest::{Client, Method};
use serde::Serialize;
use tauri::{AppHandle, State};

pub struct AppState {
    pub http: Client,
    /// Gefundenes Update zwischen Pruefung und Installation.
    pub pending_update: std::sync::Mutex<Option<tauri_plugin_updater::Update>>,
}

#[derive(Serialize)]
struct AppInfo {
    version: String,
    server_url: Option<String>,
    has_token: bool,
}

#[tauri::command]
fn app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        server_url: net::stored_server(&app),
        has_token: net::stored_token(&app).is_some(),
    }
}

/// Prueft eine (noch nicht gespeicherte) Server-Adresse. Gibt die Health-Antwort zurueck.
#[tauri::command]
async fn test_connection(state: State<'_, AppState>, url: String) -> CmdResult<HealthResponse> {
    let base = net::normalize_url(&url)?;
    net::health(&state.http, &base).await
}

/// Speichert die Server-Adresse. Bei einer anderen Adresse wird der Token verworfen.
#[tauri::command]
fn save_server_url(app: AppHandle, url: String) -> CmdResult<String> {
    let base = net::normalize_url(&url)?;
    if net::stored_server(&app).as_deref() != Some(base.as_str()) {
        net::save_token(&app, None)?;
    }
    net::save_server(&app, &base)?;
    Ok(base)
}

#[tauri::command]
async fn health(app: AppHandle, state: State<'_, AppState>) -> CmdResult<HealthResponse> {
    let base = net::require_server(&app)?;
    net::health(&state.http, &base).await
}

#[tauri::command]
async fn login(
    app: AppHandle,
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> CmdResult<UserInfo> {
    let base = net::require_server(&app)?;
    let device_name = gethostname::gethostname().to_string_lossy().into_owned();
    let req = LoginRequest { username: username.trim().to_owned(), password, device_name: Some(device_name) };
    let resp: LoginResponse = net::post_json_anon(&state.http, &base, "/auth/login", &req)
        .await
        .map_err(|mut e| {
            if e.code == "unauthorized" {
                e.message = "Benutzername oder Passwort ist falsch.".to_owned();
            }
            e
        })?;
    net::save_token(&app, Some(&resp.token))?;
    Ok(resp.user)
}

#[tauri::command]
async fn logout(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    net::logout_remote(&app, &state.http).await;
    net::save_token(&app, None)
}

#[tauri::command]
async fn me(app: AppHandle, state: State<'_, AppState>) -> CmdResult<UserInfo> {
    net::authed(&app, &state.http, Method::GET, "/me", None::<&()>).await
}

#[tauri::command]
async fn chat(
    app: AppHandle,
    state: State<'_, AppState>,
    question: String,
    history: Vec<ChatTurn>,
) -> CmdResult<ChatResponse> {
    if question.trim().is_empty() {
        return Err(err("invalid_request", "Bitte eine Frage eingeben."));
    }
    let body = ChatRequest { question, history };
    net::authed(&app, &state.http, Method::POST, "/chat", Some(&body)).await
}

fn main() {
    let state = AppState { http: net::build_client(), pending_update: std::sync::Mutex::new(None) };
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            app_info,
            test_connection,
            save_server_url,
            health,
            login,
            logout,
            me,
            chat,
            updates::check_update,
            updates::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("GaLa-AI konnte nicht gestartet werden");
}
