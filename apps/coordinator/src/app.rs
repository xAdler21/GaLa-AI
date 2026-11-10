//! Router-Aufbau (ohne Datenbankzugriff beim Bauen, damit testbar).

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tower_http::trace::TraceLayer;

use crate::error::ApiErr;
use crate::state::AppState;
use crate::{admin, api, internal, public};

async fn fallback(uri: Uri) -> Response {
    if uri.path().starts_with("/api/") {
        ApiErr::not_found("Route nicht gefunden").into_response()
    } else {
        (StatusCode::NOT_FOUND, "Nicht gefunden").into_response()
    }
}

/// Öffentlicher Listener: Desktop-API, Web-Seiten, Admin-Panel. Kein `/internal`.
pub fn public_router(state: AppState) -> Router {
    Router::new()
        .merge(public::router())
        .merge(api::router(state.clone()))
        .merge(admin::router(state.clone()))
        .fallback(fallback)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Interner Listener: ausschließlich `/internal/*`.
pub fn internal_router(state: AppState) -> Router {
    internal::router()
        .fallback(|| async { (StatusCode::NOT_FOUND, "Nicht gefunden") })
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    /// Zustand mit „fauler“ Datenbankverbindung: getestet werden nur Pfade ohne DB-Zugriff.
    fn state(dir: &std::path::Path) -> AppState {
        let pool = db::connect_lazy("postgres://gala:x@127.0.0.1:1/gala").unwrap();
        AppState::new(pool, Config::for_tests(dir))
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gala-coord-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn call(app: Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
        let resp = app.oneshot(req).await.unwrap();
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
        (
            parts.status,
            parts.headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    #[tokio::test]
    async fn invalid_login_body_is_json_api_error() {
        let d = tmp("login");
        let req = Request::post("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{kaputt"))
            .unwrap();
        let (status, _, body) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let e: api_types::client::ApiError = serde_json::from_str(&body).unwrap();
        assert_eq!(e.code, "bad_request");
    }

    #[tokio::test]
    async fn invalid_update_version_is_400() {
        let d = tmp("upd");
        let req = Request::get("/api/v1/updates/windows/x86_64/nix")
            .body(Body::empty())
            .unwrap();
        let (status, _, body) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("bad_request"));
    }

    #[tokio::test]
    async fn unknown_api_route_is_json_404() {
        let d = tmp("404");
        let req = Request::get("/api/v1/gibtsnicht")
            .body(Body::empty())
            .unwrap();
        let (status, _, body) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("not_found"));
    }

    #[tokio::test]
    async fn internal_routes_are_not_on_public_listener_and_vice_versa() {
        let d = tmp("split");
        let req = Request::get("/internal/claude-access")
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        for path in ["/", "/admin", "/api/v1/health", "/download", "/ca.crt"] {
            let req = Request::get(path).body(Body::empty()).unwrap();
            let (status, _, _) = call(internal_router(state(&d)), req).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn internal_maintenance_needs_token() {
        let d = tmp("int");
        for token in [None, Some("falsch")] {
            let mut b = Request::post("/internal/maintenance")
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(t) = token {
                b = b.header("x-gala-internal-token", t);
            }
            let req = b.body(Body::from(r#"{"enabled":false}"#)).unwrap();
            let (status, _, _) = call(internal_router(state(&d)), req).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn admin_requires_login_and_checks_origin() {
        let d = tmp("admin");
        let req = Request::get("/admin").body(Body::empty()).unwrap();
        let (status, headers, _) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "/admin/login");

        // POST ohne Origin/Referer
        let req = Request::post("/admin/maintenance")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("enabled=true"))
            .unwrap();
        let (status, _, _) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // fremder Origin
        let req = Request::post("/admin/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("origin", "https://evil.example")
            .header(header::HOST, "gala.test")
            .body(Body::from("username=a&password=b"))
            .unwrap();
        let (status, _, _) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // passender Origin, aber nicht angemeldet: Weiterleitung statt 403
        let req = Request::post("/admin/maintenance")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("origin", "https://gala.test")
            .body(Body::from("enabled=true"))
            .unwrap();
        let (status, headers, _) = call(public_router(state(&d)), req).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "/admin/login");
    }

    #[tokio::test]
    async fn ca_cert_and_static_files() {
        let d = tmp("ca");
        let (status, _, _) = call(
            public_router(state(&d)),
            Request::get("/ca.crt").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        std::fs::create_dir_all(d.join("data")).unwrap();
        std::fs::write(d.join("data").join("ca.crt"), "CERT").unwrap();
        let (status, headers, body) = call(
            public_router(state(&d)),
            Request::get("/ca.crt").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "CERT");
        assert!(
            headers[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment")
        );

        let (status, headers, _) = call(
            public_router(state(&d)),
            Request::get("/static/style.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/css")
        );
    }
}
