//! REST-Verträge zwischen Desktop-App (Tauri + Leptos) und dem Coordinator.
//!
//! Alle Routen liegen unter `/api/v1`. Authentifizierung per `Authorization: Bearer <token>`
//! aus [`LoginResponse`]. Fehler kommen immer als [`ApiError`] mit passendem HTTP-Status.
//! Im Wartungsmodus antworten alle Routen außer `health` und `auth/*` mit 503 und
//! `ApiError { code: "maintenance", .. }`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `GET /api/v1/health` – ohne Login, die App fragt das regelmäßig ab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Version des Coordinators (Cargo-Paketversion).
    pub server_version: String,
    pub maintenance: MaintenanceInfo,
    /// Niedrigste Desktop-Version, die der Server noch bedient. Ältere Apps erzwingen das Update.
    pub min_client_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MaintenanceInfo {
    pub enabled: bool,
    /// Text für Banner/Sperrbildschirm, z. B. „Update läuft, ca. 10 Minuten“.
    pub message: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Kalkulator,
}

/// `POST /api/v1/auth/login`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// Freitext zur Anzeige in der Sitzungsliste, z. B. Rechnername.
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub user: UserInfo,
}

/// `GET /api/v1/me`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub role: Role,
}

/// `POST /api/v1/chat` – Frage an den Dokumenten-Chatbot (Netzlaufwerk-Index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub question: String,
    /// Vorherige Runden derselben Unterhaltung, älteste zuerst.
    #[serde(default)]
    pub history: Vec<ChatTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub answer: String,
    pub sources: Vec<ChatSource>,
}

/// Fundstelle, auf die sich die Antwort stützt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSource {
    /// Pfad relativ zur Wurzel des Netzlaufwerks, z. B. `Angebote/2019/Kita.pdf`.
    pub path: String,
    /// 1-basiert; `None` bei Dateien ohne Seiten.
    pub page: Option<u32>,
    pub snippet: String,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Maschinenlesbar, z. B. `unauthorized`, `maintenance`, `not_found`, `not_implemented`.
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&Role::Kalkulator).unwrap(),
            "\"kalkulator\""
        );
    }

    #[test]
    fn chat_request_history_defaults_to_empty() {
        let req: ChatRequest =
            serde_json::from_str(r#"{"question":"Wo ist das LV Kita?"}"#).unwrap();
        assert!(req.history.is_empty());
    }
}
