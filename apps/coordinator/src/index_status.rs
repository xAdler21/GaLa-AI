//! Zustand des Dokumenten-Index für das Admin-Panel: Worker, Warteschlange, Dokumente.

use chrono::{DateTime, Duration, Utc};
use db::index_status::{DocumentCount, TaskCount, WorkerStatus};

use crate::state::AppState;
use crate::templates::{IndexStatusView, QueueRowView, WorkerView, fmt_dt};

/// Ein Worker gilt als ausgefallen, wenn so lange kein Lebenszeichen kam (er meldet sich
/// alle 30 Sekunden).
const ALIVE_SECS: i64 = 90;

/// Arten der Aufgaben in fester Reihenfolge: (Kennung, Anzeige, zuständige Rolle).
const KINDS: [(&str, &str, &str); 3] = [
    ("extract", "Text extrahieren", "cpu"),
    ("ocr_page", "OCR (Seiten)", "vision"),
    ("embed", "Embeddings", "vision"),
];

pub async fn load(state: &AppState) -> Result<IndexStatusView, db::SqlxError> {
    let workers = db::index_status::workers(&state.db).await?;
    let tasks = db::index_status::open_task_counts(&state.db).await?;
    let docs = db::index_status::document_counts(&state.db).await?;
    Ok(build(&workers, &tasks, &docs, Utc::now()))
}

fn build(
    workers: &[WorkerStatus],
    tasks: &[TaskCount],
    docs: &[DocumentCount],
    now: DateTime<Utc>,
) -> IndexStatusView {
    let alive = |w: &WorkerStatus| now - w.last_seen < Duration::seconds(ALIVE_SECS);
    let count = |kind: &str, status: &str| {
        tasks
            .iter()
            .filter(|t| t.kind == kind && t.status == status)
            .map(|t| t.count)
            .sum::<i64>()
    };
    let doc_count = |status: &str| {
        docs.iter()
            .filter(|d| d.status == status)
            .map(|d| d.count)
            .sum::<i64>()
    };

    let queue: Vec<QueueRowView> = KINDS
        .iter()
        .map(|(kind, label, _)| QueueRowView {
            label: (*label).to_string(),
            pending: count(kind, "pending"),
            running: count(kind, "running"),
            failed: count(kind, "failed"),
        })
        .collect();

    // Aufgaben warten, aber niemand, der sie bearbeiten könnte, meldet sich.
    let mut warnings = Vec::new();
    for role in ["cpu", "vision"] {
        let waiting: i64 = KINDS
            .iter()
            .filter(|(_, _, r)| *r == role)
            .map(|(kind, _, _)| count(kind, "pending") + count(kind, "running"))
            .sum();
        let has_worker = workers.iter().any(|w| w.role == role && alive(w));
        if waiting > 0 && !has_worker {
            let host = if role == "cpu" {
                "gala-coordinator"
            } else {
                "gala-ocr"
            };
            warnings.push(format!(
                "{waiting} Aufgaben warten auf einen Worker der Rolle {role}, aber keiner meldet sich ({host} prüfen)."
            ));
        }
    }

    IndexStatusView {
        workers: workers
            .iter()
            .map(|w| WorkerView {
                name: w.name.clone(),
                role: w.role.clone(),
                device: w.ocr_device.clone().unwrap_or_else(|| "–".into()),
                build_rev: w.build_rev.clone().unwrap_or_else(|| "–".into()),
                started_at: fmt_dt(w.started_at),
                last_seen: fmt_dt(w.last_seen),
                alive: alive(w),
            })
            .collect(),
        queue,
        warnings,
        docs_total: docs.iter().map(|d| d.count).sum(),
        docs_indexed: doc_count("indexed"),
        docs_in_progress: doc_count("new") + doc_count("extracting"),
        docs_failed: doc_count("failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str, role: &str, device: Option<&str>, seen_secs_ago: i64) -> WorkerStatus {
        let now = Utc::now();
        WorkerStatus {
            name: name.into(),
            role: role.into(),
            ocr_device: device.map(str::to_string),
            build_rev: None,
            started_at: now - Duration::hours(1),
            last_seen: now - Duration::seconds(seen_secs_ago),
        }
    }

    fn task(kind: &str, status: &str, count: i64) -> TaskCount {
        TaskCount {
            kind: kind.into(),
            status: status.into(),
            count,
        }
    }

    #[test]
    fn counts_and_alive() {
        let workers = [
            worker("gala-coordinator-cpu", "cpu", None, 10),
            worker("gala-ocr-vision", "vision", Some("cuda:0"), 20),
        ];
        let tasks = [
            task("ocr_page", "pending", 40),
            task("ocr_page", "running", 1),
            task("embed", "failed", 2),
        ];
        let docs = [
            DocumentCount {
                status: "indexed".into(),
                count: 7,
            },
            DocumentCount {
                status: "extracting".into(),
                count: 3,
            },
        ];
        let v = build(&workers, &tasks, &docs, Utc::now());
        assert!(v.warnings.is_empty());
        assert!(v.workers.iter().all(|w| w.alive));
        assert_eq!(v.workers[1].device, "cuda:0");
        assert_eq!(v.workers[0].device, "–");
        let ocr = &v.queue[1];
        assert_eq!((ocr.pending, ocr.running, ocr.failed), (40, 1, 0));
        assert_eq!(v.queue[2].failed, 2);
        assert_eq!(
            (
                v.docs_total,
                v.docs_indexed,
                v.docs_in_progress,
                v.docs_failed
            ),
            (10, 7, 3, 0)
        );
    }

    #[test]
    fn warns_when_no_worker_for_waiting_tasks() {
        // Der vision-Worker hat sich zu lange nicht gemeldet, cpu fehlt ganz.
        let workers = [worker("gala-ocr-vision", "vision", Some("cpu"), 600)];
        let tasks = [
            task("ocr_page", "pending", 5),
            task("extract", "pending", 1),
        ];
        let v = build(&workers, &tasks, &[], Utc::now());
        assert!(!v.workers[0].alive);
        assert_eq!(v.warnings.len(), 2);
        assert!(v.warnings.iter().any(|w| w.contains("vision")));
        assert!(v.warnings.iter().any(|w| w.contains("gala-coordinator")));
    }

    #[test]
    fn no_warning_without_tasks() {
        let v = build(&[], &[], &[], Utc::now());
        assert!(v.warnings.is_empty());
        assert_eq!(v.queue.len(), 3);
    }
}
