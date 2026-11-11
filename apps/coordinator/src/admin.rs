//! Admin-Panel (server-gerendert mit askama, kleine HTMX-Teile).
//!
//! Anmeldung per Cookie-Sitzung (`kind = 'web'`), nur Rolle `admin`. CSRF-Schutz:
//! `SameSite=Strict` plus Prüfung von `Origin`/`Referer` bei allen schreibenden Anfragen.

use axum::extract::{DefaultBodyLimit, FromRequestParts, Multipart, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::{Duration, Utc};
use db::settings::ClaudeAccess;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{self, ClientIp};
use crate::files;
use crate::index_status;
use crate::maintenance;
use crate::public::{PLATFORM, render};
use crate::server_update::{self, RequestError};
use crate::state::AppState;
use crate::templates::{
    AuditView, ClaudeView, DashboardTemplate, IndexStatusTemplate, LoginTemplate, MaintenanceView,
    ReleaseView, UpdateStatusTemplate, UpdateStatusView, UserView, fmt_dt,
};

pub const COOKIE_NAME: &str = "gala_admin";
/// Gültigkeit einer Admin-Websitzung.
const SESSION_HOURS: i64 = 12;
/// Maximale Größe eines Release-Uploads (Installer).
const UPLOAD_LIMIT: usize = 500 * 1024 * 1024;
/// Obergrenze für die Signaturdatei.
const SIG_LIMIT: usize = 64 * 1024;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/login", get(login_page).post(login))
        .route("/admin/logout", post(logout))
        .route("/admin/maintenance", post(set_maintenance))
        .route("/admin/claude-access", post(set_claude_access))
        .route("/admin/update", post(start_update))
        .route("/admin/update/status", get(update_status))
        .route("/admin/index/status", get(index_status_fragment))
        .route(
            "/admin/releases",
            post(upload_release).layer(DefaultBodyLimit::max(UPLOAD_LIMIT + 1024 * 1024)),
        )
        .route("/admin/releases/{id}/publish", post(publish_release))
        .route("/admin/releases/{id}/unpublish", post(unpublish_release))
        .route("/admin/releases/{id}/delete", post(delete_release))
        .route("/admin/users", post(create_user))
        .route("/admin/users/{id}/disable", post(disable_user))
        .route("/admin/users/{id}/enable", post(enable_user))
        .route("/admin/users/{id}/password", post(set_user_password))
        .layer(middleware::from_fn_with_state(state, origin_guard))
}

// ---------------------------------------------------------------------------------------------
// CSRF: Origin/Referer prüfen
// ---------------------------------------------------------------------------------------------

fn authority_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next()?;
    (!auth.is_empty()).then_some(auth)
}

/// Stimmt der Host von `Origin` (ersatzweise `Referer`) mit dem Host der Anfrage oder der
/// öffentlichen URL überein? Fehlen beide Header, wird abgelehnt.
pub fn origin_allowed(
    origin: Option<&str>,
    referer: Option<&str>,
    host: Option<&str>,
    public_authority: Option<&str>,
) -> bool {
    let Some(source) = origin.filter(|o| *o != "null").or(referer) else {
        return false;
    };
    let Some(auth) = authority_of(source) else {
        return false;
    };
    let same = |other: Option<&str>| other.is_some_and(|o| o.eq_ignore_ascii_case(auth));
    same(host) || same(public_authority)
}

async fn origin_guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        let h = req.headers();
        let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok());
        if !origin_allowed(
            get("origin"),
            get("referer"),
            get("host"),
            state.cfg.public_authority(),
        ) {
            tracing::warn!(
                path = req.uri().path(),
                "Anfrage mit fremdem Ursprung abgelehnt"
            );
            return (StatusCode::FORBIDDEN, "Ungültiger Ursprung der Anfrage").into_response();
        }
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------------------------
// Sitzung / Cookie
// ---------------------------------------------------------------------------------------------

pub fn session_cookie(token: &str, max_age_secs: i64, secure: bool) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}"
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

pub fn clear_cookie(secure: bool) -> String {
    session_cookie("", 0, secure)
}

fn with_cookie(cookie: String, resp: impl IntoResponse) -> Response {
    let mut resp = resp.into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// Angemeldeter Admin; sonst Weiterleitung zur Anmeldung.
pub struct AdminSession {
    pub user_id: Uuid,
    pub username: String,
}

impl FromRequestParts<AppState> for AdminSession {
    type Rejection = Redirect;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Redirect> {
        let login = || Redirect::to("/admin/login");
        let token = auth::cookie_value(&parts.headers, COOKIE_NAME)
            .filter(|t| !t.is_empty())
            .ok_or_else(login)?;
        let s = db::sessions::find_valid(&state.db, &auth::hash_token(token), "web")
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Sitzung nicht lesbar");
                login()
            })?
            .ok_or_else(login)?;
        if s.role != "admin" {
            return Err(login());
        }
        Ok(Self {
            user_id: s.user_id,
            username: s.username,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Hilfen: Meldungen per Redirect
// ---------------------------------------------------------------------------------------------

/// Prozent-kodiert für Query-Werte.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn ok(anchor: &str, msg: &str) -> Response {
    Redirect::to(&format!("/admin?msg={}#{anchor}", urlencode(msg))).into_response()
}

fn fail(anchor: &str, msg: &str) -> Response {
    Redirect::to(&format!("/admin?err={}#{anchor}", urlencode(msg))).into_response()
}

fn internal_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "{what}");
    fail("", "Interner Fehler, Details im Server-Log.")
}

// ---------------------------------------------------------------------------------------------
// Anmeldung
// ---------------------------------------------------------------------------------------------

async fn login_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = auth::cookie_value(&headers, COOKIE_NAME)
        && let Ok(Some(s)) =
            db::sessions::find_valid(&state.db, &auth::hash_token(token), "web").await
        && s.role == "admin"
    {
        return Redirect::to("/admin").into_response();
    }
    render(&LoginTemplate { error: None })
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

fn login_error(status: StatusCode, msg: &str) -> Response {
    let mut r = render(&LoginTemplate {
        error: Some(msg.into()),
    });
    *r.status_mut() = status;
    r
}

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Form(f): Form<LoginForm>,
) -> Response {
    let username = f.username.trim().to_string();
    if let Some(wait) = state.limiter.blocked(&username, &ip) {
        return login_error(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "Zu viele Fehlversuche, bitte in {} Minuten erneut versuchen.",
                wait.as_secs().div_ceil(60).max(1)
            ),
        );
    }
    let user = match db::users::find_by_username(&state.db, &username).await {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(error = %e, "Login: DB-Fehler");
            return login_error(StatusCode::INTERNAL_SERVER_ERROR, "Interner Fehler.");
        }
    };
    let hash = user
        .as_ref()
        .map_or_else(|| auth::dummy_hash().to_string(), |u| u.pw_hash.clone());
    let password = f.password;
    let password_ok = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash))
        .await
        .unwrap_or(false);

    let user = match user {
        Some(u) if password_ok && !u.disabled && u.role == "admin" => u,
        _ => {
            state.limiter.record_failure(&username, &ip);
            db::audit::record(
                &state.db,
                None,
                "login.failed",
                serde_json::json!({ "username": username, "ip": ip, "kind": "web" }),
            )
            .await;
            return login_error(
                StatusCode::UNAUTHORIZED,
                "Benutzername oder Passwort falsch (oder kein Admin).",
            );
        }
    };
    state.limiter.clear(&username, &ip);

    let token = auth::new_token();
    let expires = Utc::now() + Duration::hours(SESSION_HOURS);
    if let Err(e) = db::sessions::create(
        &state.db,
        &auth::hash_token(&token),
        user.id,
        "web",
        None,
        expires,
    )
    .await
    {
        tracing::error!(error = %e, "Login: Sitzung nicht speicherbar");
        return login_error(StatusCode::INTERNAL_SERVER_ERROR, "Interner Fehler.");
    }
    db::audit::record(
        &state.db,
        Some(user.id),
        "login",
        serde_json::json!({ "kind": "web", "ip": ip }),
    )
    .await;
    with_cookie(
        session_cookie(&token, SESSION_HOURS * 3600, state.cfg.cookie_secure),
        Redirect::to("/admin"),
    )
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = auth::cookie_value(&headers, COOKIE_NAME) {
        let _ = db::sessions::delete(&state.db, &auth::hash_token(token)).await;
    }
    with_cookie(
        clear_cookie(state.cfg.cookie_secure),
        Redirect::to("/admin/login"),
    )
}

// ---------------------------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct Flash {
    msg: Option<String>,
    err: Option<String>,
}

fn state_label(state: &str) -> &'static str {
    match state {
        "running" => "läuft",
        "succeeded" => "erfolgreich",
        "failed" => "fehlgeschlagen",
        _ => "bereit",
    }
}

fn update_view(state: &AppState) -> UpdateStatusView {
    let dir = &state.cfg.state_dir;
    let s = server_update::read_status(dir);
    let log = s.log_text();
    let known = ["idle", "running", "succeeded", "failed"];
    UpdateStatusView {
        state_label: state_label(&s.state).to_string(),
        running: s.is_running(),
        pending: server_update::request_pending(dir),
        state: if known.contains(&s.state.as_str()) {
            s.state
        } else {
            "idle".into()
        },
        git_ref: s.git_ref.filter(|v| !v.is_empty()),
        started_at: s.started_at.filter(|v| !v.is_empty()),
        finished_at: s.finished_at.filter(|v| !v.is_empty()),
        message: s.message.filter(|v| !v.is_empty()),
        log,
    }
}

async fn dashboard(
    State(state): State<AppState>,
    admin: AdminSession,
    Query(flash): Query<Flash>,
) -> Response {
    match build_dashboard(&state, &admin, flash).await {
        Ok(t) => render(&t),
        Err(e) => {
            tracing::error!(error = %e, "Dashboard: DB-Fehler");
            (StatusCode::INTERNAL_SERVER_ERROR, "Interner Serverfehler").into_response()
        }
    }
}

async fn build_dashboard(
    state: &AppState,
    admin: &AdminSession,
    flash: Flash,
) -> Result<DashboardTemplate, db::SqlxError> {
    let m = db::settings::get_maintenance(&state.db).await?;
    let c = db::settings::get_claude_access(&state.db).await?;
    let releases = db::releases::list(&state.db).await?;
    let users = db::users::list(&state.db).await?;
    let audit = db::audit::recent(&state.db, 50).await?;
    let index = index_status::load(state).await?;
    Ok(DashboardTemplate {
        username: admin.username.clone(),
        flash: flash.msg.filter(|s| !s.is_empty()),
        error: flash.err.filter(|s| !s.is_empty()),
        maintenance: MaintenanceView {
            enabled: m.enabled,
            message: m.message.unwrap_or_default(),
            since: m.since.map(fmt_dt),
        },
        claude: ClaudeView {
            enabled: c.enabled,
            changed_at: c.changed_at.map(fmt_dt),
            changed_by: c.changed_by,
        },
        update: update_view(state),
        index,
        releases: releases
            .into_iter()
            .map(|r| ReleaseView {
                id: r.id.to_string(),
                version: r.version,
                file_name: r.file_name,
                notes: r.notes,
                published: r.published,
                created_at: fmt_dt(r.created_at),
                published_at: r.published_at.map(fmt_dt),
            })
            .collect(),
        users: users
            .into_iter()
            .map(|u| UserView {
                is_self: u.id == admin.user_id,
                id: u.id.to_string(),
                username: u.username,
                role: u.role,
                disabled: u.disabled,
                created_at: fmt_dt(u.created_at),
            })
            .collect(),
        audit: audit
            .into_iter()
            .map(|a| AuditView {
                at: fmt_dt(a.at),
                who: a.username.unwrap_or_else(|| "–".into()),
                action: a.action,
                detail: a.detail.to_string(),
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------------------------
// Wartungsmodus und Claude-Zugang
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct MaintenanceForm {
    enabled: bool,
    #[serde(default)]
    message: Option<String>,
}

async fn set_maintenance(
    State(state): State<AppState>,
    admin: AdminSession,
    Form(f): Form<MaintenanceForm>,
) -> Response {
    match maintenance::set(
        &state,
        f.enabled,
        f.message,
        Some((admin.user_id, &admin.username)),
        "admin",
    )
    .await
    {
        Ok(m) => ok(
            "wartung",
            if m.enabled {
                "Wartungsmodus eingeschaltet."
            } else {
                "Wartungsmodus beendet."
            },
        ),
        Err(e) => internal_error("Wartungsmodus", e),
    }
}

#[derive(Deserialize)]
struct ClaudeForm {
    enabled: bool,
}

async fn set_claude_access(
    State(state): State<AppState>,
    admin: AdminSession,
    Form(f): Form<ClaudeForm>,
) -> Response {
    let value = ClaudeAccess {
        enabled: f.enabled,
        changed_at: Some(Utc::now()),
        changed_by: Some(admin.username.clone()),
    };
    if let Err(e) = db::settings::set_claude_access(&state.db, &value, Some(&admin.username)).await
    {
        return internal_error("Claude-Zugang", e);
    }
    db::audit::record(
        &state.db,
        Some(admin.user_id),
        "claude_access.set",
        serde_json::json!({ "enabled": f.enabled }),
    )
    .await;
    ok(
        "claude",
        if f.enabled {
            "Claude-Zugang eingeschaltet."
        } else {
            "Claude-Zugang ausgeschaltet."
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Server-Update
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct UpdateForm {
    git_ref: String,
    #[serde(default)]
    maintenance: Option<String>,
}

async fn start_update(
    State(state): State<AppState>,
    admin: AdminSession,
    Form(f): Form<UpdateForm>,
) -> Response {
    let git_ref = f.git_ref.trim().to_string();
    let dir = state.cfg.state_dir.clone();
    let with_maintenance = f.maintenance.is_some();

    if let Err(e) = server_update::preflight(&dir, &git_ref) {
        return fail("update", &e.to_string());
    }
    // Wartungsmodus zuerst, damit während des Updates niemand mehr arbeitet.
    let mut turned_on = false;
    if with_maintenance {
        match db::settings::get_maintenance(&state.db).await {
            Ok(m) if !m.enabled => {
                if let Err(e) = maintenance::set(
                    &state,
                    true,
                    Some("Server-Update läuft, bitte einen Moment Geduld.".into()),
                    Some((admin.user_id, &admin.username)),
                    "update",
                )
                .await
                {
                    return internal_error("Wartungsmodus", e);
                }
                turned_on = true;
            }
            Ok(_) => {}
            Err(e) => return internal_error("Wartungsmodus", e),
        }
    }

    let username = admin.username.clone();
    let r = git_ref.clone();
    let result = tokio::task::spawn_blocking(move || {
        server_update::write_request(&dir, &username, &r, with_maintenance)
    })
    .await
    .unwrap_or_else(|e| Err(RequestError::Io(e.to_string())));
    match result {
        Ok(_) => {
            db::audit::record(
                &state.db,
                Some(admin.user_id),
                "update.request",
                serde_json::json!({ "ref": git_ref, "maintenance": with_maintenance }),
            )
            .await;
            ok("update", "Update angefordert.")
        }
        Err(e) => {
            if turned_on {
                let _ = maintenance::set(
                    &state,
                    false,
                    None,
                    Some((admin.user_id, &admin.username)),
                    "update",
                )
                .await;
            }
            fail("update", &e.to_string())
        }
    }
}

async fn update_status(State(state): State<AppState>, _admin: AdminSession) -> Response {
    render(&UpdateStatusTemplate {
        update: update_view(&state),
    })
}

/// HTMX-Fragment: Worker, Warteschlange und Dokumente des Index.
async fn index_status_fragment(State(state): State<AppState>, _admin: AdminSession) -> Response {
    match index_status::load(&state).await {
        Ok(index) => render(&IndexStatusTemplate { index }),
        Err(e) => {
            tracing::error!(error = %e, "Index-Status: DB-Fehler");
            (StatusCode::INTERNAL_SERVER_ERROR, "Interner Serverfehler").into_response()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------------------------

/// Ergebnis des Upload-Parsens.
#[derive(Default)]
struct Upload {
    version: String,
    notes: String,
    signature: String,
    signature_file: String,
    installer_name: Option<String>,
    installer_received: bool,
}

/// Prüft die Versionsangabe: SemVer ohne Build-Metadaten, kanonisch geschrieben.
pub fn validate_version(v: &str) -> Result<String, String> {
    let v = v.trim();
    let parsed = semver::Version::parse(v)
        .map_err(|_| "Ungültige Version (erwartet SemVer, z. B. 0.2.0).".to_string())?;
    if !parsed.build.is_empty() || parsed.to_string() != v || !files::is_safe_component(v) {
        return Err("Ungültige Version (ohne Build-Metadaten, z. B. 0.2.0).".into());
    }
    Ok(v.to_string())
}

async fn read_upload(mut mp: Multipart, tmp_file: &std::path::Path) -> Result<Upload, String> {
    use tokio::io::AsyncWriteExt;
    let mperr = |e: axum::extract::multipart::MultipartError| format!("Upload fehlgeschlagen: {e}");
    let mut up = Upload::default();
    while let Some(mut field) = mp.next_field().await.map_err(mperr)? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "version" => up.version = field.text().await.map_err(mperr)?,
            "notes" => up.notes = field.text().await.map_err(mperr)?,
            "signature" => up.signature = field.text().await.map_err(mperr)?,
            "signature_file" => {
                let mut buf = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(mperr)? {
                    buf.extend_from_slice(&chunk);
                    if buf.len() > SIG_LIMIT {
                        return Err("Signaturdatei ist zu groß.".into());
                    }
                }
                up.signature_file = String::from_utf8_lossy(&buf).into_owned();
            }
            "installer" => {
                up.installer_name = field.file_name().map(str::to_string);
                let mut file = tokio::fs::File::create(tmp_file)
                    .await
                    .map_err(|e| format!("Datei nicht schreibbar: {e}"))?;
                let mut size: usize = 0;
                while let Some(chunk) = field.chunk().await.map_err(mperr)? {
                    size += chunk.len();
                    if size > UPLOAD_LIMIT {
                        return Err("Installer ist größer als 500 MB.".into());
                    }
                    file.write_all(&chunk)
                        .await
                        .map_err(|e| format!("Datei nicht schreibbar: {e}"))?;
                }
                file.flush()
                    .await
                    .map_err(|e| format!("Datei nicht schreibbar: {e}"))?;
                up.installer_received = size > 0;
            }
            _ => {}
        }
    }
    Ok(up)
}

async fn process_upload(
    state: &AppState,
    admin: &AdminSession,
    mp: Multipart,
    tmp_dir: &std::path::Path,
) -> Result<String, String> {
    let io = |e: std::io::Error| format!("Dateifehler: {e}");
    tokio::fs::create_dir_all(tmp_dir).await.map_err(io)?;
    let tmp_file = tmp_dir.join("installer.part");
    let up = read_upload(mp, &tmp_file).await?;

    let version = validate_version(&up.version)?;
    if !up.installer_received {
        return Err("Bitte einen Installer auswählen.".into());
    }
    let file_name = up
        .installer_name
        .as_deref()
        .and_then(files::sanitize_file_name)
        .filter(|n| n.to_ascii_lowercase().ends_with(".exe"))
        .ok_or("Der Installer muss eine .exe-Datei sein.")?;
    let signature = if up.signature.trim().is_empty() {
        up.signature_file.trim()
    } else {
        up.signature.trim()
    }
    .to_string();
    if signature.is_empty() {
        return Err("Signatur fehlt (Text oder .sig-Datei).".into());
    }
    if db::releases::get_by_version(&state.db, &version)
        .await
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("Version {version} existiert bereits."));
    }

    let target_dir = files::safe_release_path(&state.cfg.releases_dir(), &version, &file_name)
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .ok_or("Unsicherer Pfad.")?;
    tokio::fs::create_dir_all(&target_dir).await.map_err(io)?;
    let target = target_dir.join(&file_name);
    let _ = tokio::fs::remove_file(&target).await;
    tokio::fs::rename(&tmp_file, &target).await.map_err(io)?;

    let rec = db::releases::NewRelease {
        version: &version,
        platform: PLATFORM,
        file_name: &file_name,
        signature: &signature,
        notes: up.notes.trim(),
        created_by: Some(&admin.username),
    };
    if let Err(e) = db::releases::insert(&state.db, &rec).await {
        let _ = tokio::fs::remove_dir_all(&target_dir).await;
        return Err(if db::is_unique_violation(&e) {
            format!("Version {version} existiert bereits.")
        } else {
            tracing::error!(error = %e, "Release nicht speicherbar");
            "Release konnte nicht gespeichert werden.".into()
        });
    }
    db::audit::record(
        &state.db,
        Some(admin.user_id),
        "release.upload",
        serde_json::json!({ "version": version, "file": file_name }),
    )
    .await;
    Ok(version)
}

async fn upload_release(
    State(state): State<AppState>,
    admin: AdminSession,
    mp: Multipart,
) -> Response {
    let tmp_dir = state
        .cfg
        .releases_dir()
        .join(format!(".upload-{}", &auth::new_token()[..16]));
    let result = process_upload(&state, &admin, mp, &tmp_dir).await;
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    match result {
        Ok(v) => ok(
            "releases",
            &format!("Release {v} hochgeladen. Es ist noch nicht veröffentlicht."),
        ),
        Err(msg) => fail("releases", &msg),
    }
}

async fn set_published(
    state: &AppState,
    admin: &AdminSession,
    id: &str,
    published: bool,
) -> Response {
    let Ok(id) = id.parse::<Uuid>() else {
        return fail("releases", "Ungültige Release-ID.");
    };
    match db::releases::get(&state.db, id).await {
        Ok(Some(r)) => match db::releases::set_published(&state.db, id, published).await {
            Ok(_) => {
                db::audit::record(
                    &state.db,
                    Some(admin.user_id),
                    if published {
                        "release.publish"
                    } else {
                        "release.unpublish"
                    },
                    serde_json::json!({ "version": r.version }),
                )
                .await;
                ok(
                    "releases",
                    &format!(
                        "Release {} {}.",
                        r.version,
                        if published {
                            "veröffentlicht"
                        } else {
                            "zurückgezogen"
                        }
                    ),
                )
            }
            Err(e) => internal_error("Release", e),
        },
        Ok(None) => fail("releases", "Release nicht gefunden."),
        Err(e) => internal_error("Release", e),
    }
}

async fn publish_release(
    State(state): State<AppState>,
    admin: AdminSession,
    Path(id): Path<String>,
) -> Response {
    set_published(&state, &admin, &id, true).await
}

async fn unpublish_release(
    State(state): State<AppState>,
    admin: AdminSession,
    Path(id): Path<String>,
) -> Response {
    set_published(&state, &admin, &id, false).await
}

async fn delete_release(
    State(state): State<AppState>,
    admin: AdminSession,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<Uuid>() else {
        return fail("releases", "Ungültige Release-ID.");
    };
    let rel = match db::releases::get(&state.db, id).await {
        Ok(Some(r)) => r,
        Ok(None) => return fail("releases", "Release nicht gefunden."),
        Err(e) => return internal_error("Release", e),
    };
    if let Err(e) = db::releases::delete(&state.db, id).await {
        return internal_error("Release", e);
    }
    if files::is_safe_component(&rel.version) {
        let dir = state.cfg.releases_dir().join(&rel.version);
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, dir = %dir.display(), "Release-Dateien nicht löschbar");
        }
    }
    db::audit::record(
        &state.db,
        Some(admin.user_id),
        "release.delete",
        serde_json::json!({ "version": rel.version }),
    )
    .await;
    ok("releases", &format!("Release {} gelöscht.", rel.version))
}

// ---------------------------------------------------------------------------------------------
// Benutzer
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewUserForm {
    username: String,
    password: String,
    role: String,
}

/// Erlaubte Benutzernamen: 1–64 Zeichen, Buchstaben/Ziffern und `. _ - @`.
pub fn valid_username(u: &str) -> bool {
    !u.is_empty()
        && u.chars().count() <= 64
        && u.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

async fn hash_blocking(password: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

async fn create_user(
    State(state): State<AppState>,
    admin: AdminSession,
    Form(f): Form<NewUserForm>,
) -> Response {
    let username = f.username.trim().to_string();
    if !valid_username(&username) {
        return fail(
            "users",
            "Ungültiger Benutzername (Buchstaben, Ziffern, . _ - @).",
        );
    }
    if f.password.chars().count() < auth::MIN_PASSWORD_LEN {
        return fail(
            "users",
            &format!(
                "Das Passwort braucht mindestens {} Zeichen.",
                auth::MIN_PASSWORD_LEN
            ),
        );
    }
    if !matches!(f.role.as_str(), "admin" | "kalkulator") {
        return fail("users", "Ungültige Rolle.");
    }
    let hash = match hash_blocking(f.password).await {
        Ok(h) => h,
        Err(e) => return internal_error("Passwort-Hash", e),
    };
    match db::users::create(&state.db, &username, &hash, &f.role).await {
        Ok(u) => {
            db::audit::record(
                &state.db,
                Some(admin.user_id),
                "user.create",
                serde_json::json!({ "username": u.username, "role": u.role }),
            )
            .await;
            ok("users", &format!("Benutzer {} angelegt.", u.username))
        }
        Err(e) if db::is_unique_violation(&e) => {
            fail("users", "Der Benutzername ist bereits vergeben.")
        }
        Err(e) => internal_error("Benutzer anlegen", e),
    }
}

async fn change_disabled(
    state: &AppState,
    admin: &AdminSession,
    id: &str,
    disabled: bool,
) -> Response {
    let Ok(id) = id.parse::<Uuid>() else {
        return fail("users", "Ungültige Benutzer-ID.");
    };
    if disabled && id == admin.user_id {
        return fail("users", "Der eigene Zugang kann nicht gesperrt werden.");
    }
    let user = match db::users::find_by_id(&state.db, id).await {
        Ok(Some(u)) => u,
        Ok(None) => return fail("users", "Benutzer nicht gefunden."),
        Err(e) => return internal_error("Benutzer", e),
    };
    if disabled && user.role == "admin" && !user.disabled {
        match db::users::count_active_admins(&state.db).await {
            Ok(n) if n <= 1 => {
                return fail(
                    "users",
                    "Der letzte aktive Admin kann nicht gesperrt werden.",
                );
            }
            Ok(_) => {}
            Err(e) => return internal_error("Benutzer", e),
        }
    }
    match db::users::set_disabled(&state.db, id, disabled).await {
        Ok(_) => {
            db::audit::record(
                &state.db,
                Some(admin.user_id),
                if disabled {
                    "user.disable"
                } else {
                    "user.enable"
                },
                serde_json::json!({ "username": user.username }),
            )
            .await;
            ok(
                "users",
                &format!(
                    "Benutzer {} {}.",
                    user.username,
                    if disabled { "gesperrt" } else { "entsperrt" }
                ),
            )
        }
        Err(e) => internal_error("Benutzer", e),
    }
}

async fn disable_user(
    State(state): State<AppState>,
    admin: AdminSession,
    Path(id): Path<String>,
) -> Response {
    change_disabled(&state, &admin, &id, true).await
}

async fn enable_user(
    State(state): State<AppState>,
    admin: AdminSession,
    Path(id): Path<String>,
) -> Response {
    change_disabled(&state, &admin, &id, false).await
}

#[derive(Deserialize)]
struct PasswordForm {
    password: String,
}

async fn set_user_password(
    State(state): State<AppState>,
    admin: AdminSession,
    Path(id): Path<String>,
    Form(f): Form<PasswordForm>,
) -> Response {
    let Ok(id) = id.parse::<Uuid>() else {
        return fail("users", "Ungültige Benutzer-ID.");
    };
    if f.password.chars().count() < auth::MIN_PASSWORD_LEN {
        return fail(
            "users",
            &format!(
                "Das Passwort braucht mindestens {} Zeichen.",
                auth::MIN_PASSWORD_LEN
            ),
        );
    }
    let user = match db::users::find_by_id(&state.db, id).await {
        Ok(Some(u)) => u,
        Ok(None) => return fail("users", "Benutzer nicht gefunden."),
        Err(e) => return internal_error("Benutzer", e),
    };
    let hash = match hash_blocking(f.password).await {
        Ok(h) => h,
        Err(e) => return internal_error("Passwort-Hash", e),
    };
    match db::users::set_password_by_id(&state.db, id, &hash).await {
        Ok(_) => {
            db::audit::record(
                &state.db,
                Some(admin.user_id),
                "user.set_password",
                serde_json::json!({ "username": user.username }),
            )
            .await;
            // Das Zurücksetzen beendet auch die eigene Sitzung, wenn der Admin sich selbst ändert.
            if id == admin.user_id {
                return with_cookie(
                    clear_cookie(state.cfg.cookie_secure),
                    Redirect::to("/admin/login"),
                );
            }
            ok(
                "users",
                &format!("Passwort von {} gesetzt; Sitzungen beendet.", user.username),
            )
        }
        Err(e) => internal_error("Passwort", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_matches_host() {
        assert!(origin_allowed(
            Some("https://gala"),
            None,
            Some("gala"),
            None
        ));
        assert!(origin_allowed(
            Some("http://localhost:8080"),
            None,
            Some("localhost:8080"),
            None
        ));
        assert!(origin_allowed(
            None,
            Some("https://gala/admin?x=1"),
            Some("gala"),
            None
        ));
        assert!(origin_allowed(
            Some("https://GALA"),
            None,
            Some("gala"),
            None
        ));
    }

    #[test]
    fn origin_matches_public_url_behind_proxy() {
        assert!(origin_allowed(
            Some("https://gala-coordinator"),
            None,
            Some("127.0.0.1:8080"),
            Some("gala-coordinator")
        ));
    }

    #[test]
    fn foreign_or_missing_origin_is_rejected() {
        assert!(!origin_allowed(
            Some("https://evil.example"),
            None,
            Some("gala"),
            Some("gala")
        ));
        assert!(!origin_allowed(
            Some("https://gala.evil.example"),
            None,
            Some("gala"),
            None
        ));
        assert!(!origin_allowed(None, None, Some("gala"), Some("gala")));
        assert!(!origin_allowed(Some("null"), None, Some("gala"), None));
        assert!(!origin_allowed(Some("kaputt"), None, Some("gala"), None));
        // Origin hat Vorrang vor Referer
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("https://gala/"),
            Some("gala"),
            None
        ));
        // "null"-Origin fällt auf den Referer zurück
        assert!(origin_allowed(
            Some("null"),
            Some("https://gala/"),
            Some("gala"),
            None
        ));
    }

    #[test]
    fn cookie_attributes() {
        let c = session_cookie("abc", 100, true);
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict") && c.contains("Secure"));
        assert!(c.starts_with("gala_admin=abc;"));
        assert!(!session_cookie("abc", 100, false).contains("Secure"));
        assert!(clear_cookie(true).contains("Max-Age=0"));
    }

    #[test]
    fn urlencoding() {
        assert_eq!(
            urlencode("Wartung läuft & so"),
            "Wartung%20l%C3%A4uft%20%26%20so"
        );
        assert_eq!(urlencode("a-b_c.d~"), "a-b_c.d~");
    }

    #[test]
    fn version_validation() {
        assert_eq!(validate_version(" 1.2.3 ").unwrap(), "1.2.3");
        assert!(validate_version("1.2.3-beta.1").is_ok());
        for bad in [
            "",
            "1.2",
            "v1.2.3",
            "1.2.3+build",
            "../1.2.3",
            "01.2.3",
            "1.2.3/x",
        ] {
            assert!(validate_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn username_validation() {
        assert!(valid_username("anna.mueller"));
        assert!(valid_username("büro@firma"));
        assert!(!valid_username(""));
        assert!(!valid_username("a b"));
        assert!(!valid_username("<x>"));
    }
}
