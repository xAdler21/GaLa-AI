//! Öffentliche Seiten: Startseite, Installer-Download, Release-Dateien, CA-Zertifikat, Assets.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::state::AppState;
use crate::templates::{self, IndexTemplate};
use crate::{files, updates};

/// Plattform des Windows-Installers.
pub const PLATFORM: &str = "windows-x86_64";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/download", get(download))
        .route("/releases/{version}/{file}", get(release_file))
        .route("/ca.crt", get(ca_crt))
        .route("/static/style.css", get(style))
        .route("/healthz", get(|| async { "ok" }))
}

pub fn render<T: Template>(t: &T) -> Response {
    match t.render() {
        Ok(body) => Html(body).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Template-Fehler");
            (StatusCode::INTERNAL_SERVER_ERROR, "Interner Serverfehler").into_response()
        }
    }
}

async fn index(State(state): State<AppState>) -> Response {
    let releases = db::releases::list_published(&state.db, PLATFORM)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "Releases nicht lesbar");
            Vec::new()
        });
    let latest = updates::newest(&releases).map(|(_, r)| r);
    render(&IndexTemplate {
        version: latest.map(|r| r.version.clone()),
        notes: latest.map(|r| r.notes.clone()).unwrap_or_default(),
        pub_date: latest.and_then(|r| r.published_at).map(templates::fmt_dt),
        has_ca: state.cfg.ca_cert_path().is_file(),
        ca_sha1: std::fs::read_to_string(state.cfg.ca_cert_path())
            .ok()
            .and_then(|p| ca_sha1(&p)),
    })
}

async fn download(State(state): State<AppState>) -> Response {
    let releases = match db::releases::list_published(&state.db, PLATFORM).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Releases nicht lesbar");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Interner Serverfehler").into_response();
        }
    };
    match updates::newest(&releases) {
        Some((_, r)) => serve_release(&state, &r.version, &r.file_name).await,
        None => (StatusCode::NOT_FOUND, "Noch kein Installer veröffentlicht").into_response(),
    }
}

async fn release_file(
    State(state): State<AppState>,
    Path((version, file)): Path<(String, String)>,
) -> Response {
    // Nur veröffentlichte Releases, und nur die Datei, die zum Release gehört.
    match db::releases::get_by_version(&state.db, &version).await {
        Ok(Some(r)) if r.published && r.file_name == file => {
            serve_release(&state, &r.version, &r.file_name).await
        }
        Ok(_) => (StatusCode::NOT_FOUND, "Nicht gefunden").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Release nicht lesbar");
            (StatusCode::INTERNAL_SERVER_ERROR, "Interner Serverfehler").into_response()
        }
    }
}

async fn serve_release(state: &AppState, version: &str, file: &str) -> Response {
    match files::safe_release_path(&state.cfg.releases_dir(), version, file) {
        Some(path) => files::download(&path, file, "application/octet-stream").await,
        None => (StatusCode::NOT_FOUND, "Nicht gefunden").into_response(),
    }
}

async fn ca_crt(State(state): State<AppState>) -> Response {
    let path = state.cfg.ca_cert_path();
    if !path.is_file() {
        return (StatusCode::NOT_FOUND, "Kein CA-Zertifikat hinterlegt").into_response();
    }
    files::download(&path, "gala-ca.crt", "application/x-x509-ca-cert").await
}

async fn style() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            ),
        ],
        include_str!("../static/style.css"),
    )
        .into_response()
}

/// SHA-1 über das DER des (ersten) Zertifikats in `pem`, in Vierergruppen wie im
/// Windows-Sicherheitsdialog ("5711 50EF …"). `None`, wenn kein PEM-Zertifikat.
fn ca_sha1(pem: &str) -> Option<String> {
    use base64::Engine as _;
    use sha1::Digest as _;
    let body = pem
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?;
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let hex = hex::encode_upper(sha1::Sha1::digest(&der));
    Some(
        hex.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    /// Root-CA von gala-coordinator; SHA-1 laut certutil 57:11:50:EF:…:28:36.
    const PEM: &str = "-----BEGIN CERTIFICATE-----MIIBozCCAUmgAwIBAgIQCL2Hu+yn+vJdpS2LtwfWfDAKBggqhkjOPQQDAjAwMS4wLAYDVQQDEyVDYWRkeSBMb2NhbCBBdXRob3JpdHkgLSAyMDI2IEVDQyBSb290MB4XDTI2MTExMDE3MDUwNloXDTM2MDkxODE3MDUwNlowMDEuMCwGA1UEAxMlQ2FkZHkgTG9jYWwgQXV0aG9yaXR5IC0gMjAyNiBFQ0MgUm9vdDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABKg/NUIDdtv61eWF4LFyR4iY3hZVQ8fw7HkK8c4pciVZgT8kGCy0pmoIumwtTLQqAwqa6rQvjZ33Xw9hDmYMUrujRTBDMA4GA1UdDwEB/wQEAwIBBjASBgNVHRMBAf8ECDAGAQH/AgEBMB0GA1UdDgQWBBQcCERNawJORYSOSaklkfHyiVhDbTAKBggqhkjOPQQDAgNIADBFAiBLkxGUHpozXZfemVDRZFwEnAzmYOFnHei2z1uQ614NnQIhAPPsJts3SPk4h4HFOXk/sIgzPuUqg9roZq4wixG9CXTj-----END CERTIFICATE-----";

    #[test]
    fn ca_fingerprint() {
        assert_eq!(
            super::ca_sha1(PEM).as_deref(),
            Some("5711 50EF 3A81 AB13 E953 44B7 6323 C0BB 1E90 2836")
        );
        assert_eq!(super::ca_sha1("kein Zertifikat"), None);
    }
}
