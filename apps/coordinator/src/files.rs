//! Dateizugriff für Releases: Pfad-Sicherheit, Dateinamen bereinigen, Dateien ausliefern.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio_util::io::ReaderStream;

/// Ist `s` ein harmloser einzelner Pfadbestandteil (Version oder Dateiname)?
pub fn is_safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('.')
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Setzt `<root>/<version>/<file>` zusammen – oder `None`, wenn ein Teil unsicher ist.
pub fn safe_release_path(root: &Path, version: &str, file: &str) -> Option<PathBuf> {
    (is_safe_component(version) && is_safe_component(file)).then(|| root.join(version).join(file))
}

/// Macht aus einem hochgeladenen Dateinamen einen sicheren Namen (nur letzter Bestandteil,
/// Sonderzeichen werden zu `_`).
pub fn sanitize_file_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.').replace("..", "_");
    (!cleaned.is_empty() && cleaned.len() <= 200).then_some(cleaned)
}

/// Liefert eine Datei als Download (`Content-Disposition: attachment`).
pub async fn download(path: &Path, download_name: &str, content_type: &'static str) -> Response {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "Datei nicht gefunden").into_response(),
    };
    let len = file.metadata().await.map(|m| m.len()).ok();
    let mut resp = Response::new(Body::from_stream(ReaderStream::new(file)));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Some(len) = len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    // download_name ist bereits bereinigt (nur [A-Za-z0-9._-]), sonst Fallback.
    let disp = format!("attachment; filename=\"{download_name}\"");
    if let Ok(v) = HeaderValue::from_str(&disp) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_components() {
        assert!(is_safe_component("0.1.0"));
        assert!(is_safe_component("GaLa-AI_0.1.0_x64-setup.exe"));
        for bad in [
            "", "..", ".", ".hidden", "a/b", "a\\b", "a..b", "a b", "C:", "%2e%2e", "a\0b",
        ] {
            assert!(!is_safe_component(bad), "{bad:?}");
        }
    }

    #[test]
    fn path_traversal_is_rejected() {
        let root = Path::new("/srv/gala/data/releases");
        assert!(safe_release_path(root, "0.1.0", "setup.exe").is_some());
        assert!(safe_release_path(root, "..", "setup.exe").is_none());
        assert!(safe_release_path(root, "0.1.0", "../../etc/passwd").is_none());
        assert!(safe_release_path(root, "0.1.0", "..\\boot.ini").is_none());
    }

    #[test]
    fn sanitizing_upload_names() {
        assert_eq!(
            sanitize_file_name("GaLa-AI_0.1.0_x64-setup.exe").unwrap(),
            "GaLa-AI_0.1.0_x64-setup.exe"
        );
        assert_eq!(
            sanitize_file_name("C:\\Users\\x\\Neu Setup.exe").unwrap(),
            "Neu_Setup.exe"
        );
        assert_eq!(sanitize_file_name("../../evil.exe").unwrap(), "evil.exe");
        assert_eq!(
            sanitize_file_name("..").map(|s| is_safe_component(&s)),
            None
        );
        assert_eq!(sanitize_file_name(""), None);
        // Ergebnis ist immer ein sicherer Bestandteil
        for n in ["a..b.exe", "..a", "ä.exe", "x y.exe"] {
            let s = sanitize_file_name(n).unwrap();
            assert!(is_safe_component(&s), "{n} -> {s}");
        }
    }
}
