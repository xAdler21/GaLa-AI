//! Gemeinsamer Zustand der Oberflaeche und die Ablaeufe (Start, Health-Abfrage, Update-Pruefung).

use api_types::client::{ApiError, ChatResponse, HealthResponse, MaintenanceInfo, UserInfo};
use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::tauri::{self, UpdateInfo};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Boot,
    Setup,
    Login,
    Main,
    Unreachable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Chat,
    Offers,
    Settings,
}

#[derive(Clone, PartialEq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(UpdateInfo),
    Downloading(u64, Option<u64>),
    Failed(String),
}

#[derive(Clone, PartialEq)]
pub enum ChatState {
    Pending,
    Done(ChatResponse),
    NotImplemented,
    Failed(String),
}

#[derive(Clone, Copy)]
pub struct ChatEntry {
    pub id: usize,
    pub question: StoredValue<String>,
    pub state: RwSignal<ChatState>,
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub screen: RwSignal<Screen>,
    pub page: RwSignal<Page>,
    pub health: RwSignal<Option<HealthResponse>>,
    pub offline: RwSignal<bool>,
    pub version: RwSignal<String>,
    pub server: RwSignal<String>,
    pub user: RwSignal<Option<UserInfo>>,
    pub notice: RwSignal<Option<String>>,
    pub fatal: RwSignal<String>,
    pub update: RwSignal<UpdateState>,
    pub chat: RwSignal<Vec<ChatEntry>>,
}

impl Ctx {
    pub fn new() -> Self {
        Self {
            screen: RwSignal::new(Screen::Boot),
            page: RwSignal::new(Page::Chat),
            health: RwSignal::new(None),
            offline: RwSignal::new(false),
            version: RwSignal::new(String::new()),
            server: RwSignal::new(String::new()),
            user: RwSignal::new(None),
            notice: RwSignal::new(None),
            fatal: RwSignal::new(String::new()),
            update: RwSignal::new(UpdateState::Idle),
            chat: RwSignal::new(Vec::new()),
        }
    }

    /// Aktive Wartung laut letzter Health-Antwort.
    pub fn maintenance(&self) -> Option<MaintenanceInfo> {
        self.health.get().map(|h| h.maintenance).filter(|m| m.enabled)
    }

    /// Server verlangt eine neuere App-Version, als wir haben.
    pub fn update_required(&self) -> bool {
        let Some(min) = self.health.get().and_then(|h| h.min_client_version) else {
            return false;
        };
        let parse = |s: &str| semver::Version::parse(s.trim().trim_start_matches('v')).ok();
        match (parse(&min), parse(&self.version.get())) {
            (Some(min), Some(own)) => own < min,
            _ => false,
        }
    }

    /// Zentrale Fehlerbehandlung: 401 fuehrt zur Anmeldung, 503 "maintenance" loest sofort eine Health-Abfrage aus.
    pub fn handle_error(&self, e: &ApiError) {
        match e.code.as_str() {
            "unauthorized" => {
                self.user.set(None);
                self.notice.set(Some("Die Sitzung ist abgelaufen. Bitte erneut anmelden.".into()));
                self.screen.set(Screen::Login);
            }
            "maintenance" => {
                let ctx = *self;
                spawn_local(async move { refresh_health(ctx).await });
            }
            "no_server" => self.screen.set(Screen::Setup),
            _ => {}
        }
    }
}

pub async fn refresh_health(ctx: Ctx) {
    match tauri::health().await {
        Ok(h) => {
            ctx.offline.set(false);
            ctx.health.set(Some(h));
        }
        // Letzten Stand behalten: Eine Wartung bleibt gesperrt, auch wenn der Server gerade neu startet.
        Err(_) => ctx.offline.set(true),
    }
}

/// Dauerschleife: alle 30 s (waehrend Wartung alle 10 s) den Health-Endpunkt abfragen.
pub async fn health_loop(ctx: Ctx) {
    loop {
        let wait = if ctx.health.get_untracked().is_some_and(|h| h.maintenance.enabled) { 10_000 } else { 30_000 };
        TimeoutFuture::new(wait).await;
        if !ctx.server.get_untracked().is_empty() && ctx.screen.get_untracked() != Screen::Setup {
            refresh_health(ctx).await;
        }
    }
}

pub async fn check_update(ctx: Ctx) {
    if matches!(ctx.update.get_untracked(), UpdateState::Checking | UpdateState::Downloading(..)) {
        return;
    }
    ctx.update.set(UpdateState::Checking);
    match tauri::check_update().await {
        Ok(Some(info)) => ctx.update.set(UpdateState::Available(info)),
        Ok(None) => ctx.update.set(UpdateState::UpToDate),
        Err(e) => ctx.update.set(UpdateState::Failed(e.message)),
    }
}

pub async fn install_update(ctx: Ctx) {
    ctx.update.set(UpdateState::Downloading(0, None));
    if let Err(e) = tauri::install_update().await {
        ctx.update.set(UpdateState::Failed(e.message));
    }
    // Bei Erfolg startet die App neu.
}

/// Startablauf; auch nach dem Speichern einer neuen Server-Adresse.
pub async fn start(ctx: Ctx) {
    ctx.screen.set(Screen::Boot);
    let info = match tauri::app_info().await {
        Ok(i) => i,
        Err(e) => {
            ctx.fatal.set(e.message);
            ctx.screen.set(Screen::Unreachable);
            return;
        }
    };
    ctx.version.set(info.version);
    let Some(server) = info.server_url else {
        ctx.screen.set(Screen::Setup);
        return;
    };
    ctx.server.set(server);
    refresh_health(ctx).await;
    spawn_local(check_update(ctx));
    if !info.has_token {
        ctx.user.set(None);
        ctx.screen.set(Screen::Login);
        return;
    }
    match tauri::me().await {
        Ok(u) => {
            ctx.user.set(Some(u));
            ctx.page.set(Page::Chat);
            ctx.screen.set(Screen::Main);
        }
        Err(e) => match e.code.as_str() {
            "unauthorized" => ctx.screen.set(Screen::Login),
            // Wartung: Token ist vermutlich gueltig, ein 401 spaeter fuehrt zur Anmeldung.
            "maintenance" => ctx.screen.set(Screen::Main),
            _ => {
                ctx.fatal.set(e.message);
                ctx.screen.set(Screen::Unreachable);
            }
        },
    }
}
