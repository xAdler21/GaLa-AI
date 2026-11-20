//! REST-Verträge zwischen Desktop-App (Tauri + Leptos) und dem Coordinator.
//!
//! Alle Routen liegen unter `/api/v1`. Authentifizierung per `Authorization: Bearer <token>`
//! aus [`LoginResponse`]. Fehler kommen immer als [`ApiError`] mit passendem HTTP-Status.
//! Im Wartungsmodus antworten alle Routen außer `health` und `auth/*` mit 503 und
//! `ApiError { code: "maintenance", .. }`.

use chrono::{DateTime, Utc};
use lv_model::{Decimal, Lv, Oz};
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

/// Eintrag in `GET /api/v1/lvs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LvSummary {
    pub id: Uuid,
    pub name: String,
    /// Quellformat, z. B. `X83` oder `D83`.
    pub format: String,
    pub file_name: String,
    pub positions: u32,
    /// Positionen mit Einheitspreis.
    pub priced: u32,
    /// Angebotssumme netto.
    pub total: Decimal,
    /// Für `PUT /lvs/{id}/prices`: Speichern nur mit dieser Revision.
    pub revision: i32,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/v1/lvs` – LV-Datei importieren (GAEB X8x/D8x).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LvImportRequest {
    pub file_name: String,
    /// Dateiinhalt, Base64 (Standard-Alphabet mit Padding).
    pub data_base64: String,
}

/// `GET /api/v1/lvs/{id}`, auch Antwort auf Import und Speichern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LvDetail {
    pub summary: LvSummary,
    pub lv: Lv,
    /// Hinweise des Imports, z. B. ignorierte Satzarten.
    pub warnings: Vec<String>,
    /// Mögliche Exporte für dieses LV.
    pub exports: Vec<ExportFormat>,
}

/// `PUT /api/v1/lvs/{id}/prices` – Einheitspreise setzen; GP = Menge × EP rechnet der Server.
/// Bei veralteter `revision` antwortet er mit 409 (`conflict`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceUpdateRequest {
    pub revision: i32,
    pub prices: Vec<PriceEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceEntry {
    pub oz: Oz,
    /// `None` löscht den Preis.
    pub ep: Option<Decimal>,
}

/// `GET /api/v1/lvs/{id}/suggestions` – frühere Einheitspreise zu ähnlichen Positionen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceSuggestion {
    pub oz: Oz,
    /// Bester Treffer zuerst.
    pub refs: Vec<PriceRef>,
}

/// Ein früherer Preis aus einer GAEB-Abgabe im Archiv.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceRef {
    pub ep: Decimal,
    pub einheit: Option<String>,
    pub menge: Option<Decimal>,
    pub kurztext: String,
    pub lv_name: String,
    /// 84 Angebot, 85 Nebenangebot, 86 Auftrag.
    pub phase: u8,
    /// Pfad im Archiv.
    pub path: String,
    /// Änderungsdatum der Datei.
    pub date: DateTime<Utc>,
    /// Ähnlichkeit der Kurztexte, 0–1.
    pub similarity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// GAEB DA XML Angebotsabgabe (aus X83).
    X84,
    /// GAEB 90 Angebotsabgabe (aus D83).
    D84,
    Xlsx,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ExportFormat::X84 => "x84",
            ExportFormat::D84 => "d84",
            ExportFormat::Xlsx => "xlsx",
        }
    }

    pub fn parse(s: &str) -> Option<ExportFormat> {
        match s {
            "x84" => Some(ExportFormat::X84),
            "d84" => Some(ExportFormat::D84),
            "xlsx" => Some(ExportFormat::Xlsx),
            _ => None,
        }
    }
}

/// `GET /api/v1/lvs/{id}/export/{format}` (`x84`, `d84`, `xlsx`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LvExport {
    pub file_name: String,
    pub data_base64: String,
    /// OZ der Positionen ohne Preis (in X84/D84 ohne Preis bzw. ohne Preissatz).
    pub missing_prices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Maschinenlesbar, z. B. `unauthorized`, `maintenance`, `not_found`, `not_implemented`,
    /// `conflict`.
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
