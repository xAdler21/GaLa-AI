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

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate {
    pub username: String,
    pub flash: Option<String>,
    pub error: Option<String>,
    pub maintenance: MaintenanceView,
    pub claude: ClaudeView,
    pub update: UpdateStatusView,
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
            releases: vec![],
            users: vec![],
            audit: vec![],
        };
        let html = t.render().unwrap();
        assert!(html.contains("Wartungsmodus"));
        assert!(html.contains("Claude-Zugang"));
        assert!(html.contains("hx-trigger"));
        assert!(html.contains("zeile &lt;1&gt;") || html.contains("zeile &#60;1&#62;"));
    }
}
