//! Auto-Updater. Der Endpunkt kommt erst zur Laufzeit aus der gespeicherten Server-Adresse.

use api_types::client::ApiError;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State, Url};
use tauri_plugin_updater::UpdaterExt;

use crate::AppState;
use crate::net::{CmdResult, err, require_server};

/// Platzhalter ersetzt der Updater selbst (`{{target}}`, `{{arch}}`, `{{current_version}}`).
const ENDPOINT_PATH: &str = "/api/v1/updates/{{target}}/{{arch}}/{{current_version}}";

#[derive(Serialize)]
pub struct UpdateInfo {
    version: String,
    current_version: String,
    notes: Option<String>,
    date: Option<String>,
}

#[derive(Serialize, Clone)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

fn upd_err(e: impl std::fmt::Display) -> ApiError {
    err("update", format!("Update-Fehler: {e}"))
}

/// `Ok(None)`, wenn der Server 204 liefert (kein neueres Release).
#[tauri::command]
pub async fn check_update(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Option<UpdateInfo>> {
    let base = require_server(&app)?;
    let url = Url::parse(&format!("{base}{ENDPOINT_PATH}")).map_err(upd_err)?;
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(upd_err)?
        .build()
        .map_err(upd_err)?;
    let found = updater.check().await.map_err(upd_err)?;
    let info = found.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        notes: u.body.clone().filter(|b| !b.trim().is_empty()),
        date: u.date.map(|d| d.to_string()),
    });
    *state.pending_update.lock().expect("Mutex vergiftet") = found;
    Ok(info)
}

/// Laedt das zuvor gefundene Update (Event `update-progress`), installiert es und startet neu.
#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let update = state
        .pending_update
        .lock()
        .expect("Mutex vergiftet")
        .take()
        .ok_or_else(|| err("update", "Es wurde kein Update gefunden. Bitte erneut nach Updates suchen."))?;

    let mut downloaded: u64 = 0;
    let handle = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = handle.emit("update-progress", Progress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(upd_err)?;
    app.restart()
}
