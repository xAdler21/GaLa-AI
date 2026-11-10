//! Postgres-Pool und sqlx-Migrationen. Die Datenbank läuft auf der Coordinator-VM;
//! der Worker auf der OCR-VM verbindet sich über das LAN.
//!
//! Migrationen liegen in `crates/db/migrations/`. Nummernkreise:
//! - `0001`–`0099`: Coordinator (Nutzer, Sitzungen, Einstellungen, Releases, Audit)
//! - `0100`–`0199`: Dokumenten-Index (Dokumente, Chunks, Index-Aufgaben)
//! - ab `0200`: LV-/Preis-Domäne (M1 ff.)
//!
//! Queries laufen zur Laufzeit (`sqlx::query`), nicht über die `query!`-Makros, damit der
//! Build keine Datenbank braucht.

use anyhow::Context;
pub use sqlx::Error as SqlxError;
pub use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub mod audit;
pub mod releases;
pub mod sessions;
pub mod settings;
pub mod users;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(16)
        .connect(url)
        .await
        .context("Verbindung zu Postgres fehlgeschlagen")
}

/// Pool ohne sofortigen Verbindungsaufbau (für Tests ohne Datenbank).
pub fn connect_lazy(url: &str) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .connect_lazy(url)
        .context("Ungültige Datenbank-URL")
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("Migrationen fehlgeschlagen")?;
    tracing::info!("Datenbank-Migrationen angewendet");
    Ok(())
}

/// Verletzt der Fehler eine UNIQUE-Bedingung (z. B. doppelter Nutzername)?
pub fn is_unique_violation(e: &SqlxError) -> bool {
    e.as_database_error()
        .is_some_and(|d| d.is_unique_violation())
}
