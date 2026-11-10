//! Server-Update aus dem Admin-Panel: `update-request.json` schreiben, `update-status.json` lesen.
//!
//! Der Coordinator läuft nicht als root. Ausgeführt wird das Update von `gala-update.service`
//! (ausgelöst per `gala-update.path`), siehe CLAUDE.md.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

pub const REQUEST_FILE: &str = "update-request.json";
pub const STATUS_FILE: &str = "update-status.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub requested_by: String,
    pub requested_at: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub maintenance: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct UpdateStatus {
    #[serde(default)]
    pub state: String,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Text oder Liste von Zeilen.
    #[serde(default)]
    pub log_tail: Option<serde_json::Value>,
}

impl UpdateStatus {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }

    pub fn log_text(&self) -> String {
        match &self.log_tail {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

/// Liest den Status; fehlt die Datei oder ist sie unlesbar, gilt `idle`.
pub fn read_status(state_dir: &Path) -> UpdateStatus {
    let idle = || UpdateStatus {
        state: "idle".into(),
        ..UpdateStatus::default()
    };
    match std::fs::read(state_dir.join(STATUS_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "update-status.json unlesbar");
            idle()
        }),
        Err(_) => idle(),
    }
}

pub fn request_pending(state_dir: &Path) -> bool {
    state_dir.join(REQUEST_FILE).exists()
}

/// Erlaubte Git-Refs: Branch-/Tag-Namen oder Commit-Hashes, nichts Gefährliches für die Shell.
pub fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 100
        && !r.starts_with(['-', '/', '.'])
        && !r.ends_with(['/', '.'])
        && !r.contains("..")
        && !r.contains("//")
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

#[derive(Debug, PartialEq, Eq)]
pub enum RequestError {
    InvalidRef,
    Running,
    Pending,
    Io(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRef => f.write_str("Ungültige Git-Referenz."),
            Self::Running => f.write_str("Es läuft bereits ein Update."),
            Self::Pending => f.write_str("Es liegt bereits eine Update-Anfrage vor."),
            Self::Io(e) => write!(f, "Anfrage konnte nicht geschrieben werden: {e}"),
        }
    }
}

/// Prüft, ob eine neue Anfrage zulässig ist (gültige Referenz, nichts läuft, nichts wartet).
pub fn preflight(state_dir: &Path, git_ref: &str) -> Result<(), RequestError> {
    if !valid_ref(git_ref) {
        return Err(RequestError::InvalidRef);
    }
    if read_status(state_dir).is_running() {
        return Err(RequestError::Running);
    }
    if request_pending(state_dir) {
        return Err(RequestError::Pending);
    }
    Ok(())
}

/// Schreibt die Anfrage atomar (temporäre Datei + Umbenennen).
pub fn write_request(
    state_dir: &Path,
    requested_by: &str,
    git_ref: &str,
    maintenance: bool,
) -> Result<UpdateRequest, RequestError> {
    preflight(state_dir, git_ref)?;
    let req = UpdateRequest {
        requested_by: requested_by.to_string(),
        requested_at: Utc::now().to_rfc3339(),
        git_ref: git_ref.to_string(),
        maintenance,
    };
    let io = |e: std::io::Error| RequestError::Io(e.to_string());
    std::fs::create_dir_all(state_dir).map_err(io)?;
    let tmp: PathBuf = state_dir.join(format!(".{REQUEST_FILE}.tmp"));
    let json = serde_json::to_vec_pretty(&req).map_err(|e| RequestError::Io(e.to_string()))?;
    std::fs::write(&tmp, json).map_err(io)?;
    std::fs::rename(&tmp, state_dir.join(REQUEST_FILE)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })?;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gala-coord-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ref_validation() {
        for ok in ["main", "v1.2.3", "feature/x-y_z", "0123abcd"] {
            assert!(valid_ref(ok), "{ok}");
        }
        for bad in [
            "", "-rf", "a b", "a;b", "$(x)", "a..b", "/abs", "x/", "a\nb", "ü",
        ] {
            assert!(!valid_ref(bad), "{bad:?}");
        }
    }

    #[test]
    fn writes_request_atomically() {
        let d = tmpdir("write");
        let r = write_request(&d, "admin", "main", true).unwrap();
        assert_eq!(r.git_ref, "main");
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(d.join(REQUEST_FILE)).unwrap()).unwrap();
        assert_eq!(v["requested_by"], "admin");
        assert_eq!(v["ref"], "main");
        assert_eq!(v["maintenance"], true);
        assert!(v["requested_at"].is_string());
        // keine temporäre Datei übrig
        assert!(!d.join(format!(".{REQUEST_FILE}.tmp")).exists());
        // zweite Anfrage wird abgelehnt
        assert_eq!(
            write_request(&d, "admin", "main", false),
            Err(RequestError::Pending)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn refuses_while_running() {
        let d = tmpdir("running");
        std::fs::write(
            d.join(STATUS_FILE),
            r#"{"state":"running","ref":"main","log_tail":["a","b"]}"#,
        )
        .unwrap();
        assert_eq!(
            write_request(&d, "a", "main", true),
            Err(RequestError::Running)
        );
        let s = read_status(&d);
        assert!(s.is_running());
        assert_eq!(s.log_text(), "a\nb");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn refuses_invalid_ref_and_missing_status_is_idle() {
        let d = tmpdir("invalid");
        assert_eq!(
            write_request(&d, "a", "x;y", true),
            Err(RequestError::InvalidRef)
        );
        assert_eq!(read_status(&d).state, "idle");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn status_after_success_allows_new_request() {
        let d = tmpdir("done");
        std::fs::write(
            d.join(STATUS_FILE),
            r#"{"state":"succeeded","log_tail":"ok"}"#,
        )
        .unwrap();
        assert!(write_request(&d, "a", "main", false).is_ok());
        assert_eq!(read_status(&d).log_text(), "ok");
        let _ = std::fs::remove_dir_all(&d);
    }
}
