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
