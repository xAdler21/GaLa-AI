//! GaLa-AI Coordinator: REST-API für die Desktop-App, Web-Panel, Updates und interne API.

mod admin;
mod api;
mod app;
mod auth;
mod chatbot;
mod config;
mod error;
mod files;
mod index_status;
mod internal;
mod lvs;
mod maintenance;
mod public;
mod server_update;
mod state;
mod templates;
mod updates;

use std::net::SocketAddr;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::state::AppState;

#[derive(Parser)]
#[command(name = "coordinator", version, about = "GaLa-AI Coordinator")]
struct Cli {
    /// Postgres-Verbindung.
    #[arg(long, env = "DATABASE_URL", global = true, hide_env_values = true)]
    database_url: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Server starten (Standard). Wendet Migrationen automatisch an.
    Serve,
    /// Nur die Datenbank-Migrationen anwenden.
    Migrate,
    /// Benutzerverwaltung.
    #[command(subcommand)]
    Admin(AdminCommand),
    /// Desktop-Releases (für Veröffentlichungen per SSH, ohne Anmeldung im Admin-Panel).
    #[command(subcommand)]
    Release(ReleaseCommand),
    /// Frage an den Dokumenten-Chatbot stellen und Antwort samt Quellen ausgeben (zum Prüfen per SSH).
    Ask { question: String },
}

#[derive(Subcommand)]
enum ReleaseCommand {
    /// Installer und Signatur als Release anlegen, wie der Upload im Admin-Panel.
    Add {
        /// Version (SemVer, z. B. 0.2.0).
        #[arg(long)]
        version: String,
        /// Pfad zur `*-setup.exe`.
        #[arg(long)]
        installer: std::path::PathBuf,
        /// Pfad zur `.sig` des Installers.
        #[arg(long)]
        signature: std::path::PathBuf,
        #[arg(long, default_value = "")]
        notes: String,
        /// Sofort veröffentlichen (sonst im Admin-Panel).
        #[arg(long)]
        publish: bool,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Benutzer anlegen. Passwort aus `GALA_ADMIN_PASSWORD` oder interaktiv.
    Create {
        #[arg(long)]
        username: String,
        #[arg(long, value_enum, default_value_t = RoleArg::Admin)]
        role: RoleArg,
    },
    /// Passwort eines Benutzers setzen. Passwort aus `GALA_ADMIN_PASSWORD` oder interaktiv.
    SetPassword {
        #[arg(long)]
        username: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    Admin,
    Kalkulator,
}

impl RoleArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Kalkulator => "kalkulator",
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let database_url = cli
        .database_url
        .filter(|u| !u.trim().is_empty())
        .context("DATABASE_URL fehlt (Umgebungsvariable oder --database-url)")?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(&database_url).await,
        Command::Migrate => {
            let pool = db::connect(&database_url).await?;
            db::migrate(&pool).await
        }
        Command::Admin(cmd) => admin_command(&database_url, cmd).await,
        Command::Release(cmd) => release_command(&database_url, cmd).await,
        Command::Ask { question } => ask(&database_url, &question).await,
    }
}

async fn ask(database_url: &str, question: &str) -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let pool = db::connect(database_url).await?;
    let bot = build_chatbot(&cfg, &pool)
        .await
        .context("Chatbot nicht verfügbar (Feature `embed` oder Modell fehlt)")?;
    let resp = bot.answer(question, &[]).await?;
    println!("{}\n", resp.answer);
    for (i, s) in resp.sources.iter().enumerate() {
        println!(
            "[{}] {} S. {:?} ({:.4})\n    {}",
            i + 1,
            s.path,
            s.page,
            s.score,
            s.snippet
        );
    }
    Ok(())
}

async fn release_command(database_url: &str, cmd: ReleaseCommand) -> anyhow::Result<()> {
    let ReleaseCommand::Add {
        version,
        installer,
        signature,
        notes,
        publish,
    } = cmd;
    let version = admin::validate_version(&version).map_err(anyhow::Error::msg)?;
    let file_name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(files::sanitize_file_name)
        .filter(|n| n.to_ascii_lowercase().ends_with(".exe"))
        .context("Der Installer muss eine .exe-Datei sein")?;
    let sig = std::fs::read_to_string(&signature)
        .with_context(|| format!("{} nicht lesbar", signature.display()))?;
    let sig = sig.trim();
    if sig.is_empty() {
        bail!("Die Signaturdatei ist leer");
    }

    let cfg = Config::from_env()?;
    let pool = db::connect(database_url).await?;
    db::migrate(&pool).await?;
    if db::releases::get_by_version(&pool, &version)
        .await?
        .is_some()
    {
        bail!("Version {version} existiert bereits");
    }
    let target = files::safe_release_path(&cfg.releases_dir(), &version, &file_name)
        .context("Unsicherer Pfad")?;
    let dir = target.parent().context("Unsicherer Pfad")?;
    std::fs::create_dir_all(dir)?;
    std::fs::copy(&installer, &target)
        .with_context(|| format!("{} nicht kopierbar", installer.display()))?;

    let rec = db::releases::NewRelease {
        version: &version,
        platform: public::PLATFORM,
        file_name: &file_name,
        signature: sig,
        notes: notes.trim(),
        created_by: Some("cli"),
    };
    let rel = match db::releases::insert(&pool, &rec).await {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(dir);
            return Err(e).context("Release konnte nicht gespeichert werden");
        }
    };
    db::audit::record(
        &pool,
        None,
        "release.upload",
        serde_json::json!({ "version": version, "file": file_name, "source": "cli" }),
    )
    .await;
    if publish {
        db::releases::set_published(&pool, rel.id, true).await?;
        db::audit::record(
            &pool,
            None,
            "release.publish",
            serde_json::json!({ "version": version, "source": "cli" }),
        )
        .await;
    }
    println!(
        "Release {version} ({file_name}) angelegt{}.",
        if publish { " und veröffentlicht" } else { "" }
    );
    Ok(())
}

/// Passwort aus `GALA_ADMIN_PASSWORD` oder interaktiv (zweimal) lesen.
fn read_password() -> anyhow::Result<String> {
    let password = match std::env::var("GALA_ADMIN_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
    {
        Some(p) => p,
        None => {
            let p1 = rpassword::prompt_password("Passwort: ")?;
            let p2 = rpassword::prompt_password("Passwort wiederholen: ")?;
            if p1 != p2 {
                bail!("Die Passwörter stimmen nicht überein");
            }
            p1
        }
    };
    if password.chars().count() < auth::MIN_PASSWORD_LEN {
        bail!(
            "Das Passwort braucht mindestens {} Zeichen",
            auth::MIN_PASSWORD_LEN
        );
    }
    Ok(password)
}

async fn admin_command(database_url: &str, cmd: AdminCommand) -> anyhow::Result<()> {
    let pool = db::connect(database_url).await?;
    db::migrate(&pool).await?;
    match cmd {
        AdminCommand::Create { username, role } => {
            if !admin::valid_username(&username) {
                bail!("Ungültiger Benutzername (Buchstaben, Ziffern, . _ - @)");
            }
            let hash = auth::hash_password(&read_password()?)?;
            match db::users::create(&pool, &username, &hash, role.as_str()).await {
                Ok(u) => {
                    db::audit::record(&pool, None, "user.create", serde_json::json!({ "username": u.username, "role": u.role, "source": "cli" })).await;
                    println!("Benutzer {} ({}) angelegt.", u.username, u.role);
                }
                Err(e) if db::is_unique_violation(&e) => {
                    bail!("Der Benutzername {username} existiert bereits")
                }
                Err(e) => return Err(e).context("Benutzer konnte nicht angelegt werden"),
            }
        }
        AdminCommand::SetPassword { username } => {
            let hash = auth::hash_password(&read_password()?)?;
            if !db::users::set_password_by_username(&pool, &username, &hash).await? {
                bail!("Benutzer {username} nicht gefunden");
            }
            db::audit::record(
                &pool,
                None,
                "user.set_password",
                serde_json::json!({ "username": username, "source": "cli" }),
            )
            .await;
            println!("Passwort für {username} gesetzt, alle Sitzungen beendet.");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("Beenden angefordert");
}

/// Baut den Dokumenten-Chatbot (Embedder auf dem Coordinator, pgvector-Suche, llama-server).
/// Ohne Feature `embed` oder bei Ladefehler des Modells bleibt er aus (die API antwortet 501);
/// ein nicht erreichbares LLM zeigt sich erst bei der Anfrage (503), nicht beim Start.
#[cfg(feature = "embed")]
async fn build_chatbot(cfg: &Config, pool: &db::PgPool) -> Option<chatbot::SharedChatbot> {
    use std::sync::Arc;

    tracing::info!("Embedding-Modell (BGE-M3) für den Chatbot wird geladen");
    let cache = cfg.embed_cache_dir.clone();
    let embedder = match tokio::task::spawn_blocking(move || retrieval::FastEmbedder::new(cache))
        .await
    {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "Embedding-Modell nicht ladbar, Chatbot bleibt aus");
            return None;
        }
        Err(e) => {
            tracing::error!(error = %e, "Laden des Embedding-Modells abgestürzt, Chatbot bleibt aus");
            return None;
        }
    };
    let llm = match llm::LlmClient::new(cfg.llm_url.clone(), chatbot::LLM_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "LLM-Client nicht erzeugbar, Chatbot bleibt aus");
            return None;
        }
    };
    tracing::info!(llm = %cfg.llm_url, "Dokumenten-Chatbot aktiv");
    Some(Arc::new(chat::ChatService::new(
        embedder,
        retrieval::PgVectorStore::new(pool.clone()),
        llm,
    )))
}

#[cfg(not(feature = "embed"))]
async fn build_chatbot(_cfg: &Config, _pool: &db::PgPool) -> Option<chatbot::SharedChatbot> {
    tracing::info!("Ohne Feature `embed` gebaut: Dokumenten-Chatbot ist nicht aktiviert (501)");
    None
}

async fn serve(database_url: &str) -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let pool = db::connect(database_url).await?;
    db::migrate(&pool).await?;

    for dir in [cfg.releases_dir(), cfg.state_dir.clone()] {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(dir = %dir.display(), error = %e, "Verzeichnis nicht anlegbar");
        }
    }
    if cfg.internal_token.is_none() {
        tracing::warn!(
            "GALA_INTERNAL_TOKEN ist nicht gesetzt: schreibende /internal-Routen lehnen alles ab"
        );
    }

    let chat = build_chatbot(&cfg, &pool).await;
    let state = AppState::with_chat(pool, cfg.clone(), chat);
    let stop = CancellationToken::new();

    tokio::spawn({
        let stop = stop.clone();
        async move {
            shutdown_signal().await;
            stop.cancel();
        }
    });

    // Aufräumen: abgelaufene Sitzungen und Rate-Limit-Einträge.
    tokio::spawn({
        let (state, stop) = (state.clone(), stop.clone());
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    _ = tick.tick() => {
                        match db::sessions::delete_expired(&state.db).await {
                            Ok(n) if n > 0 => tracing::info!(n, "Abgelaufene Sitzungen gelöscht"),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "Sitzungs-Aufräumen fehlgeschlagen"),
                        }
                        state.limiter.prune();
                    }
                }
            }
        }
    });

    let public_listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("Bind auf {} fehlgeschlagen", cfg.bind))?;
    let internal_listener = tokio::net::TcpListener::bind(cfg.internal_bind)
        .await
        .with_context(|| format!("Bind auf {} fehlgeschlagen", cfg.internal_bind))?;
    tracing::info!(public = %cfg.bind, internal = %cfg.internal_bind, public_url = %cfg.public_url, "Coordinator läuft");

    let public = axum::serve(
        public_listener,
        app::public_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop.clone().cancelled_owned());
    let internal = axum::serve(internal_listener, app::internal_router(state))
        .with_graceful_shutdown(stop.clone().cancelled_owned());

    let (a, b) = tokio::join!(public, internal);
    a.context("Öffentlicher Listener beendet")?;
    b.context("Interner Listener beendet")?;
    tracing::info!("Coordinator beendet");
    Ok(())
}
