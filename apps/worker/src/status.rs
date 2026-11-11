//! Lebenszeichen für das Admin-Panel: der Worker schreibt regelmäßig seine Zeile in
//! `worker_status` (Rolle, OCR-Gerät, Build-Stand).

use std::time::Duration;

use chrono::{DateTime, Utc};
use db::index_status::{self, WorkerBeat};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::queue::sleep_or_stop;

/// Abstand der Lebenszeichen. Das Admin-Panel wertet einen Worker nach dem Dreifachen als
/// ausgefallen.
const INTERVAL: Duration = Duration::from_secs(30);
/// Zeilen von Workern, die sich so lange nicht gemeldet haben, werden beim Start entfernt.
const PRUNE_DAYS: i32 = 30;

pub struct WorkerInfo {
    pub name: String,
    /// `cpu` oder `vision`.
    pub role: &'static str,
    /// Nur Rolle vision: `cpu` oder `cuda:N`.
    pub ocr_device: Option<String>,
}

/// Commit, aus dem das Image gebaut wurde (setzt das Dockerfile als `GALA_BUILD_REV`).
fn build_rev() -> Option<String> {
    std::env::var("GALA_BUILD_REV")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Meldet den Worker an und erneuert das Lebenszeichen bis `stop`. Datenbankfehler werden nur
/// protokolliert: die Anzeige darf die Arbeit nicht aufhalten.
pub fn spawn(
    pool: PgPool,
    info: WorkerInfo,
    stop: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let started_at: DateTime<Utc> = Utc::now();
        let rev = build_rev();
        if let Err(err) = index_status::prune_workers(&pool, PRUNE_DAYS).await {
            tracing::warn!(%err, "Alte Worker-Einträge konnten nicht entfernt werden");
        }
        while !stop.is_cancelled() {
            let beat = WorkerBeat {
                name: &info.name,
                role: info.role,
                ocr_device: info.ocr_device.as_deref(),
                build_rev: rev.as_deref(),
                started_at,
            };
            if let Err(err) = index_status::worker_heartbeat(&pool, &beat).await {
                tracing::warn!(%err, "Lebenszeichen konnte nicht gespeichert werden");
            }
            sleep_or_stop(INTERVAL, &stop).await;
        }
    })
}
