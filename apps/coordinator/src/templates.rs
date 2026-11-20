//! Askama-Templates (Dateien in `apps/coordinator/templates/`) und ihre View-Strukturen.

use askama::Template;
use chrono::{DateTime, Utc};

/// Datum/Zeit für die Anzeige (UTC, deutsches Format).
pub fn fmt_dt(dt: DateTime<Utc>) -> String {
    dt.format("%d.%m.%Y %H:%M UTC").to_string()
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub version: Option<String>,
    pub notes: String,
    pub pub_date: Option<String>,
    pub has_ca: bool,
    /// SHA-1-Fingerabdruck der CA, wie ihn der Windows-Dialog beim Installieren zeigt.
    pub ca_sha1: Option<String>,
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub error: Option<String>,
}

pub struct MaintenanceView {
    pub enabled: bool,
    pub message: String,
    pub since: Option<String>,
}

pub struct ClaudeView {
    pub enabled: bool,
    pub changed_at: Option<String>,
    pub changed_by: Option<String>,
}

pub struct UpdateStatusView {
    pub state: String,
    pub state_label: String,
    pub running: bool,
    pub pending: bool,
    pub git_ref: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub message: Option<String>,
    pub log: String,
}

pub struct ReleaseView {
    pub id: String,
    pub version: String,
    pub file_name: String,
    pub notes: String,
    pub published: bool,
    pub created_at: String,
    pub published_at: Option<String>,
}

pub struct UserView {
    pub id: String,
    pub username: String,
    pub role: String,
    pub disabled: bool,
    pub is_self: bool,
    pub created_at: String,
}

pub struct AuditView {
    pub at: String,
    pub who: String,
    pub action: String,
    pub detail: String,
}

pub struct WorkerView {
    pub name: String,
    pub role: String,
    /// OCR-Gerät (`cpu`, `cuda:0`) oder „–“.
    pub device: String,
    pub build_rev: String,
    pub started_at: String,
    pub last_seen: String,
    /// Lebenszeichen ist frisch.
    pub alive: bool,
}

pub struct QueueRowView {
    pub label: String,
    pub pending: i64,
    pub running: i64,
    pub failed: i64,
    /// Erledigt in den letzten 10 Minuten bzw. der letzten Stunde.
    pub last_10m: i64,
    pub per_hour: i64,
    /// Restzeit beim Durchsatz der letzten Stunde, z. B. „ca. 3,5 h“.
    pub eta: String,
}

/// Eine Aufgabe in den Listen „Läuft gerade“, „OCR live“ und „Fehler“.
pub struct TaskLineView {
    pub kind: String,
    /// Gekürzter Pfad mit Seite; `path` ist der volle Pfad (Tooltip).
    pub place: String,
    pub path: String,
    pub worker: String,
    /// `true`: endgültig gescheitert, sonst wird es erneut versucht.
    pub final_failure: bool,
    pub attempts: i32,
    pub error: String,
    pub error_short: String,
    pub ago: String,
    /// Anfang des erkannten Textes (nur OCR).
    pub preview: String,
}

/// Zustand von llama-server.
#[derive(Default)]
pub struct LlmView {
    pub url: String,
    pub reachable: bool,
    pub busy: usize,
    pub total: usize,
    /// Kontext je Slot in Token.
    pub n_ctx: u32,
    /// Fehlermeldung, wenn nicht erreichbar.
    pub detail: String,
}

/// Eine Chat-Anfrage (ohne Fragetext).
pub struct ChatEventView {
    pub ago: String,
    pub user: String,
    pub secs: String,
    pub ok: bool,
    pub outcome: String,
    pub outcome_full: String,
}

/// Zustand des Dokumenten-Index (Worker, Warteschlange, Dokumente).
pub struct IndexStatusView {
    pub workers: Vec<WorkerView>,
    pub queue: Vec<QueueRowView>,
    pub warnings: Vec<String>,
    pub docs_total: i64,
    pub docs_indexed: i64,
    pub docs_in_progress: i64,
    pub docs_failed: i64,
    pub running: Vec<TaskLineView>,
    pub ocr_live: Vec<TaskLineView>,
    pub failures: Vec<TaskLineView>,
    pub llm: LlmView,
    pub chats: Vec<ChatEventView>,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate {
    pub username: String,
    /// Versionsnummer des Servers (`config::server_version`).
    pub server_version: String,
    pub flash: Option<String>,
    pub error: Option<String>,
    pub maintenance: MaintenanceView,
    pub claude: ClaudeView,
    pub update: UpdateStatusView,
    pub index: IndexStatusView,
    pub releases: Vec<ReleaseView>,
    pub users: Vec<UserView>,
    pub audit: Vec<AuditView>,
}

/// HTMX-Fragment für den Update-Status.
#[derive(Template)]
#[template(path = "update_status.html")]
pub struct UpdateStatusTemplate {
    pub update: UpdateStatusView,
}

/// HTMX-Fragment für den Zustand des Dokumenten-Index.
#[derive(Template)]
#[template(path = "index_status.html")]
pub struct IndexStatusTemplate {
    pub index: IndexStatusView,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_renders_without_release() {
        let html = IndexTemplate {
            version: None,
            notes: String::new(),
            pub_date: None,
            has_ca: false,
            ca_sha1: None,
        }
        .render()
        .unwrap();
        assert!(html.contains("GaLa-AI"));
        assert!(html.contains("/admin/login"));
        assert!(!html.contains("/ca.crt"));
    }

    #[test]
    fn index_renders_release_and_escapes_notes() {
        let html = IndexTemplate {
            version: Some("1.2.3".into()),
            notes: "<script>alert(1)</script>".into(),
            pub_date: Some("01.01.2026 10:00 UTC".into()),
            has_ca: true,
            ca_sha1: Some("5711 50EF".into()),
        }
        .render()
        .unwrap();
        assert!(html.contains("1.2.3"));
        assert!(html.contains("/download"));
        assert!(html.contains("/ca.crt"));
        assert!(!html.contains("<script>alert(1)"));
    }

    #[test]
    fn login_renders_error() {
        let html = LoginTemplate {
            error: Some("Falsch".into()),
        }
        .render()
        .unwrap();
        assert!(html.contains("Falsch"));
    }

    #[test]
    fn dashboard_renders() {
        let t = DashboardTemplate {
            username: "admin".into(),
            server_version: "0.1.12".into(),
            flash: Some("Gespeichert".into()),
            error: None,
            maintenance: MaintenanceView {
                enabled: true,
                message: "Update".into(),
                since: None,
            },
            claude: ClaudeView {
                enabled: false,
                changed_at: None,
                changed_by: None,
            },
            update: UpdateStatusView {
                state: "running".into(),
                state_label: "läuft".into(),
                running: true,
                pending: false,
                git_ref: Some("main".into()),
                started_at: None,
                finished_at: None,
                message: None,
                log: "zeile <1>".into(),
            },
            index: sample_index(),
            releases: vec![],
            users: vec![],
            audit: vec![],
        };
        let html = t.render().unwrap();
        assert!(html.contains("Wartungsmodus"));
        assert!(html.contains("Claude-Zugang"));
        assert!(html.contains("hx-trigger"));
        assert!(html.contains("zeile &lt;1&gt;") || html.contains("zeile &#60;1&#62;"));
        assert!(html.contains("Dokumenten-Index"));
        assert!(html.contains("/admin/index/status"));
    }

    fn sample_index() -> IndexStatusView {
        IndexStatusView {
            workers: vec![
                WorkerView {
                    name: "gala-ocr-vision".into(),
                    role: "vision".into(),
                    device: "cuda:0".into(),
                    build_rev: "0.1.12".into(),
                    started_at: "11.11.2026 08:00 UTC".into(),
                    last_seen: "11.11.2026 08:05 UTC".into(),
                    alive: true,
                },
                WorkerView {
                    name: "alt".into(),
                    role: "cpu".into(),
                    device: "–".into(),
                    build_rev: "–".into(),
                    started_at: "10.11.2026 08:00 UTC".into(),
                    last_seen: "10.11.2026 09:00 UTC".into(),
                    alive: false,
                },
            ],
            queue: vec![QueueRowView {
                label: "OCR (Seiten)".into(),
                pending: 41,
                running: 1,
                failed: 3,
                last_10m: 7,
                per_hour: 40,
                eta: "ca. 1,1 h".into(),
            }],
            warnings: vec!["Aufgaben <warten>".into()],
            docs_total: 12,
            docs_indexed: 9,
            docs_in_progress: 2,
            docs_failed: 1,
            running: vec![],
            ocr_live: vec![TaskLineView {
                kind: "OCR (Seiten)".into(),
                place: "…/Angebote/Holz.TIF, S. 1".into(),
                path: "2008/Angebote/Holz.TIF".into(),
                worker: "gala-ocr-vision".into(),
                final_failure: false,
                attempts: 1,
                error: String::new(),
                error_short: String::new(),
                ago: "vor 4 s".into(),
                preview: "PREISANFRAGE <b>".into(),
            }],
            failures: vec![TaskLineView {
                kind: "OCR (Seiten)".into(),
                place: "…/Sträußl/SCAN0252_000.tif, S. 2".into(),
                path: "x/SCAN0252_000.tif".into(),
                worker: "gala-ocr-vision".into(),
                final_failure: true,
                attempts: 3,
                error: "Unknown(24)".into(),
                error_short: "Unknown(24)".into(),
                ago: "vor 1 min".into(),
                preview: String::new(),
            }],
            llm: LlmView {
                url: "http://gala-llm:8080".into(),
                reachable: true,
                busy: 1,
                total: 2,
                n_ctx: 16384,
                detail: String::new(),
            },
            chats: vec![ChatEventView {
                ago: "vor 2 min".into(),
                user: "anna".into(),
                secs: "3,4 s".into(),
                ok: false,
                outcome: "context size".into(),
                outcome_full: "context size".into(),
            }],
        }
    }

    #[test]
    fn index_status_renders() {
        let html = IndexStatusTemplate {
            index: sample_index(),
        }
        .render()
        .unwrap();
        assert!(html.contains("gala-ocr-vision"));
        assert!(html.contains("cuda:0"));
        assert!(html.contains("aktiv"));
        assert!(html.contains("keine Meldung"));
        assert!(html.contains("OCR (Seiten)"));
        assert!(html.contains(">41<"));
        assert!(html.contains("ca. 1,1 h"));
        assert!(html.contains("Kontext je Slot 16384"));
        assert!(html.contains("context size"));
        assert!(
            html.contains("PREISANFRAGE &lt;b&gt;") || html.contains("PREISANFRAGE &#60;b&#62;")
        );
        assert!(html.contains("SCAN0252_000.tif") && html.contains("endgültig"));
        // Warnungen werden maskiert ausgegeben
        assert!(!html.contains("<warten>"));
        // lädt sich selbst nach
        assert!(html.contains("hx-get=\"/admin/index/status\""));
    }

    #[test]
    fn index_status_renders_empty() {
        let html = IndexStatusTemplate {
            index: IndexStatusView {
                workers: vec![],
                queue: vec![],
                warnings: vec![],
                docs_total: 0,
                docs_indexed: 0,
                docs_in_progress: 0,
                docs_failed: 0,
                running: vec![],
                ocr_live: vec![],
                failures: vec![],
                llm: LlmView::default(),
                chats: vec![],
            },
        }
        .render()
        .unwrap();
        assert!(html.contains("Noch kein Worker"));
        assert!(html.contains("nicht erreichbar"));
    }
}
