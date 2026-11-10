//! Gemeinsame Schleife aller Rollen: Aufgabe holen, bearbeiten, Lease verlängern, abschließen.

use std::sync::Arc;
use std::time::Duration;

use ingest::store::{self, QueueConfig};
use ingest::{Task, TaskKind};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

/// Wartezeit, wenn keine Aufgabe ansteht.
const POLL: Duration = Duration::from_secs(3);
/// Wartezeit nach einem Datenbankfehler beim Holen.
const DB_ERROR_WAIT: Duration = Duration::from_secs(10);

/// Gemeinsamer Zustand der Aufgabenbearbeitung.
pub struct Ctx {
    pub pool: PgPool,
    pub name: String,
    pub queue: QueueConfig,
}

/// Bearbeitet Aufgaben bestimmter Arten.
pub trait Handler: Send + Sync + 'static {
    fn kinds(&self) -> &'static [TaskKind];
    fn handle(&self, task: &Task) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Startet `concurrency` Schleifen und kehrt zurück, wenn alle nach `stop` beendet sind.
/// Eine laufende Aufgabe wird noch zu Ende geführt.
pub async fn run<H: Handler>(
    ctx: Arc<Ctx>,
    handler: Arc<H>,
    concurrency: usize,
    stop: CancellationToken,
) {
    let mut set = tokio::task::JoinSet::new();
    for n in 0..concurrency.max(1) {
        set.spawn(worker_loop(n, ctx.clone(), handler.clone(), stop.clone()));
    }
    while set.join_next().await.is_some() {}
}

async fn worker_loop<H: Handler>(
    n: usize,
    ctx: Arc<Ctx>,
    handler: Arc<H>,
    stop: CancellationToken,
) {
    tracing::info!(schleife = n, arten = ?handler.kinds(), "Aufgabenschleife gestartet");
    while !stop.is_cancelled() {
        match store::claim(&ctx.pool, handler.kinds(), &ctx.name, &ctx.queue).await {
            Ok(Some(task)) => process(&ctx, &handler, task, &stop).await,
            Ok(None) => sleep_or_stop(POLL, &stop).await,
            Err(err) => {
                tracing::warn!(%err, "Aufgabe konnte nicht geholt werden");
                sleep_or_stop(DB_ERROR_WAIT, &stop).await;
            }
        }
    }
    tracing::info!(schleife = n, "Aufgabenschleife beendet");
}

/// Fehler samt Ursachenkette als eine Zeile (`a: b: c`).
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

pub async fn sleep_or_stop(d: Duration, stop: &CancellationToken) {
    tokio::select! {
        () = stop.cancelled() => {},
        () = tokio::time::sleep(d) => {},
    }
}

async fn process<H: Handler>(
    ctx: &Arc<Ctx>,
    handler: &Arc<H>,
    task: Task,
    stop: &CancellationToken,
) {
    tracing::info!(task = task.id, art = task.kind.as_str(), dokument = %task.document_id,
        seite = ?task.page, versuch = task.attempts, "Aufgabe beginnt");
    let started = std::time::Instant::now();

    // Eigener tokio-Task: eine Panik im Handler wird so zum normalen Fehler.
    let mut handle = tokio::spawn({
        let (handler, task) = (handler.clone(), task.clone());
        async move { handler.handle(&task).await }
    });
    let mut beat = tokio::time::interval((ctx.queue.lease / 3).max(Duration::from_secs(1)));
    beat.tick().await;
    let mut announced_stop = false;
    let outcome = loop {
        tokio::select! {
            res = &mut handle => break res,
            _ = beat.tick() => {
                match store::heartbeat(&ctx.pool, task.id, &ctx.name, &ctx.queue).await {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(task = task.id, "Lease verloren, Aufgabe gehört nicht mehr diesem Worker"),
                    Err(err) => tracing::warn!(task = task.id, %err, "Heartbeat fehlgeschlagen"),
                }
            }
            () = stop.cancelled(), if !announced_stop => {
                announced_stop = true;
                tracing::info!(task = task.id, "Beenden angefordert, laufende Aufgabe wird noch abgeschlossen");
            }
        }
    };

    let result = match outcome {
        Ok(r) => r,
        Err(join) => Err(anyhow::anyhow!("Aufgabe abgestürzt: {join}")),
    };
    match result {
        Ok(()) => {
            if let Err(err) = store::complete(&ctx.pool, task.id).await {
                tracing::error!(task = task.id, %err, "Aufgabe konnte nicht abgeschlossen werden");
            }
            tracing::info!(
                task = task.id,
                dauer_ms = started.elapsed().as_millis() as u64,
                "Aufgabe fertig"
            );
        }
        Err(err) => {
            let message = format!("{err:#}");
            tracing::warn!(task = task.id, fehler = %message, "Aufgabe fehlgeschlagen");
            match store::fail(&ctx.pool, &task, &message, &ctx.queue).await {
                Ok(true) => on_permanent_failure(ctx, &task, &message).await,
                Ok(false) => return,
                Err(e) => {
                    tracing::error!(task = task.id, err = %e, "Fehler konnte nicht gespeichert werden");
                    return;
                }
            }
        }
    }
    if let Err(err) = store::finalize_if_idle(&ctx.pool, task.document_id).await {
        tracing::warn!(dokument = %task.document_id, %err, "Dokument konnte nicht finalisiert werden");
    }
}

/// Endgültig gescheitert: `extract` setzt das Dokument auf `failed`, andere Arten hängen nur
/// eine Notiz an, damit das Dokument nicht ewig in `extracting` bleibt.
async fn on_permanent_failure(ctx: &Ctx, task: &Task, message: &str) {
    let res = match task.kind {
        TaskKind::Extract => store::set_failed(&ctx.pool, task.document_id, message).await,
        kind => {
            let note = match task.page {
                Some(p) => format!("{} Seite {p}: {message}", kind.as_str()),
                None => format!("{}: {message}", kind.as_str()),
            };
            store::note_error(&ctx.pool, task.document_id, &note).await
        }
    };
    if let Err(err) = res {
        tracing::error!(dokument = %task.document_id, %err, "Fehlernotiz konnte nicht gespeichert werden");
    }
}
