//! Zustand des Dokumenten-Index für das Admin-Panel: Worker, Warteschlange mit Durchsatz,
//! laufende Aufgaben, die letzten OCR-Seiten, Fehler und das Sprachmodell (Chat).

use chrono::{DateTime, Duration, Utc};
use db::index_status::{DocumentCount, TaskCount, TaskLine, Throughput, WorkerStatus};

use crate::chatbot::ChatEvent;
use crate::state::AppState;
use crate::templates::{
    ChatEventView, IndexStatusView, LlmView, QueueRowView, TaskLineView, WorkerView, fmt_dt,
};

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
    let through = db::index_status::throughput(&state.db).await?;
    let now = Utc::now();
    let mut v = build(&workers, &tasks, &docs, &through, now);
    let lines = |rows: Vec<TaskLine>| rows.iter().map(|t| task_line(t, now)).collect::<Vec<_>>();
    v.running = lines(db::index_status::running_tasks(&state.db, 20).await?);
    v.ocr_live = lines(db::index_status::recent_ocr(&state.db, 10).await?);
    v.failures = lines(db::index_status::recent_failures(&state.db, 30).await?);
    v.llm = llm_view(state).await;
    v.chats = state
        .chat_log
        .recent()
        .iter()
        .map(|e| chat_view(e, now))
        .collect();
    Ok(v)
}

/// Erreichbarkeit und Slot-Belegung von llama-server (Zeitlimit 2 s).
async fn llm_view(state: &AppState) -> LlmView {
    let url = state.cfg.llm_url.clone();
    let Some(client) = &state.llm_status else {
        return LlmView {
            url,
            detail: "kein Client".into(),
            ..LlmView::default()
        };
    };
    match client.slots().await {
        Ok(slots) => LlmView {
            url,
            reachable: true,
            busy: slots.iter().filter(|s| s.is_processing).count(),
            total: slots.len(),
            n_ctx: slots.first().map_or(0, |s| s.n_ctx),
            detail: String::new(),
        },
        Err(e) => LlmView {
            url,
            detail: e.to_string(),
            ..LlmView::default()
        },
    }
}

fn label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map_or(kind, |(_, l, _)| l)
        .to_string()
}

/// „vor 12 s“, „vor 3 min“, „vor 2 h“, älter: Datum.
fn ago(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let s = (now - at).num_seconds().max(0);
    match s {
        0..60 => format!("vor {s} s"),
        60..3600 => format!("vor {} min", s / 60),
        3600..86400 => format!("vor {} h", s / 3600),
        _ => fmt_dt(at),
    }
}

/// Die letzten zwei Pfadteile, z. B. „…/Lieferanten - Angebote/Holz Walter.TIF“.
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    match parts.as_slice() {
        [file, dir, _] => format!("…/{dir}/{file}"),
        _ => path.to_string(),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn task_line(t: &TaskLine, now: DateTime<Utc>) -> TaskLineView {
    let page = t.page.map(|p| format!(", S. {p}")).unwrap_or_default();
    let error = t.error.clone().unwrap_or_default();
    TaskLineView {
        kind: label(&t.kind),
        place: format!("{}{page}", short_path(&t.rel_path)),
        path: t.rel_path.clone(),
        worker: t.worker.clone().unwrap_or_else(|| "–".into()),
        final_failure: t.status == "failed",
        attempts: t.attempts,
        error_short: clip(&error, 220),
        error,
        ago: ago(now, t.updated_at),
        preview: t.preview.clone().unwrap_or_default(),
    }
}

fn chat_view(e: &ChatEvent, now: DateTime<Utc>) -> ChatEventView {
    ChatEventView {
        ago: ago(now, e.at),
        user: e.user.clone(),
        secs: format!("{:.1} s", e.millis as f64 / 1000.0).replace('.', ","),
        ok: e.ok,
        outcome: clip(&e.outcome, 300),
        outcome_full: e.outcome.clone(),
    }
}

/// Restzeit aus offenen Aufgaben und dem Durchsatz der letzten Stunde.
fn eta(open: i64, per_hour: i64) -> String {
    if open == 0 {
        return "–".into();
    }
    if per_hour == 0 {
        return "steht".into();
    }
    let hours = open as f64 / per_hour as f64;
    if hours < 1.0 {
        format!("ca. {} min", (hours * 60.0).ceil() as i64)
    } else if hours < 48.0 {
        format!("ca. {} h", format!("{hours:.1}").replace('.', ","))
    } else {
        format!("ca. {} Tage", (hours / 24.0).round() as i64)
    }
}

fn build(
    workers: &[WorkerStatus],
    tasks: &[TaskCount],
    docs: &[DocumentCount],
    through: &[Throughput],
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
        .map(|(kind, label, _)| {
            let t = through.iter().find(|t| t.kind == *kind);
            let per_hour = t.map_or(0, |t| t.last_1h);
            let pending = count(kind, "pending");
            let running = count(kind, "running");
            QueueRowView {
                label: (*label).to_string(),
                pending,
                running,
                failed: count(kind, "failed"),
                last_10m: t.map_or(0, |t| t.last_10m),
                per_hour,
                eta: eta(pending + running, per_hour),
            }
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
        running: Vec::new(),
        ocr_live: Vec::new(),
        failures: Vec::new(),
        llm: LlmView::default(),
        chats: Vec::new(),
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
        let v = build(&workers, &tasks, &docs, &[], Utc::now());
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
        let v = build(&workers, &tasks, &[], &[], Utc::now());
        assert!(!v.workers[0].alive);
        assert_eq!(v.warnings.len(), 2);
        assert!(v.warnings.iter().any(|w| w.contains("vision")));
        assert!(v.warnings.iter().any(|w| w.contains("gala-coordinator")));
    }

    #[test]
    fn no_warning_without_tasks() {
        let v = build(&[], &[], &[], &[], Utc::now());
        assert!(v.warnings.is_empty());
        assert_eq!(v.queue.len(), 3);
    }

    #[test]
    fn throughput_and_eta() {
        let tasks = [
            task("ocr_page", "pending", 89),
            task("ocr_page", "running", 1),
        ];
        let through = [Throughput {
            kind: "ocr_page".into(),
            last_10m: 12,
            last_1h: 60,
        }];
        let v = build(&[], &tasks, &[], &through, Utc::now());
        let ocr = &v.queue[1];
        assert_eq!((ocr.last_10m, ocr.per_hour), (12, 60));
        assert_eq!(ocr.eta, "ca. 1,5 h");
        assert_eq!(v.queue[0].eta, "–");
        assert_eq!(eta(5, 0), "steht");
        assert_eq!(eta(30, 60), "ca. 30 min");
        assert_eq!(eta(4800, 50), "ca. 4 Tage");
    }

    #[test]
    fn short_paths_and_ago() {
        assert_eq!(
            short_path("2008/Mücke/Angebote/Holz.TIF"),
            "…/Angebote/Holz.TIF"
        );
        assert_eq!(short_path("a/b.pdf"), "a/b.pdf");
        let now = Utc::now();
        assert_eq!(ago(now, now - Duration::seconds(5)), "vor 5 s");
        assert_eq!(ago(now, now - Duration::minutes(3)), "vor 3 min");
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
