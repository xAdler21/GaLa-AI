//! Eigener Hintergrund-Thread für Objekte, die nicht zwischen Threads wandern dürfen
//! (PDFium ist nicht thread-safe, OCR-Engines halten GPU-Zustand).
//!
//! Das Objekt wird im Thread erzeugt und lebt dort; Aufrufe laufen als Closures nacheinander.

use std::panic::AssertUnwindSafe;
use std::sync::mpsc;

use anyhow::{Context, anyhow};

type Job<T> = Box<dyn FnOnce(&T) + Send>;

pub struct Actor<T> {
    tx: mpsc::Sender<Job<T>>,
}

impl<T: 'static> Actor<T> {
    /// Startet den Thread und wartet, bis `init` das Objekt erzeugt hat (z. B. Modelle geladen).
    pub fn spawn(
        name: &str,
        init: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<Job<T>>();
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let value = match init() {
                    Ok(v) => {
                        let _ = ready_tx.send(Ok(()));
                        v
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while let Ok(job) = rx.recv() {
                    if std::panic::catch_unwind(AssertUnwindSafe(|| job(&value))).is_err() {
                        tracing::error!("Panik im Hintergrund-Thread, Aufruf verworfen");
                    }
                }
            })
            .with_context(|| format!("Thread {name} nicht startbar"))?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("Thread {name} ist beim Start abgestürzt"))??;
        Ok(Self { tx })
    }

    /// Führt `f` im Thread aus und wartet asynchron auf das Ergebnis.
    pub async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&T) -> R + Send + 'static,
    ) -> anyhow::Result<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Box::new(move |value| {
                let _ = tx.send(f(value));
            }))
            .map_err(|_| anyhow!("Hintergrund-Thread ist beendet"))?;
        rx.await
            .map_err(|_| anyhow!("Hintergrund-Aufruf ist abgebrochen (Panik?)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_jobs_in_thread_and_survives_panic() {
        let actor = Actor::spawn("test", || Ok(41_u32)).unwrap();
        assert_eq!(actor.call(|v| *v + 1).await.unwrap(), 42);
        assert!(actor.call(|_| panic!("absichtlich")).await.is_err());
        assert_eq!(actor.call(|v| *v).await.unwrap(), 41);
    }

    #[test]
    fn init_error_is_reported() {
        let r = Actor::<u32>::spawn("test", || Err(anyhow!("kaputt")));
        assert!(r.is_err());
    }
}
