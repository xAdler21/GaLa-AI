//! Dokumenten-Chatbot im Coordinator: Anbindung von `chat::ChatService` und Fehlerabbildung.
//!
//! `ChatService` ist über Embedder/Suche/LLM generisch (nicht objektsicher); hier wird er hinter
//! [`Chatbot`] versteckt, damit `AppState` ihn ohne Typparameter halten und Tests ihn ersetzen können.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::Notify;

use api_types::client::{ChatRequest, ChatResponse, ChatStreamEvent, ChatTurn};
use axum::body::{Body, Bytes};
use chat::{ChatError, ChatService, Llm, Rerank, Search, Sink};
use llm::LlmError;
use retrieval::Embedder;

use crate::error::ApiErr;

/// Meldung, wenn der Server ohne Embedder (Feature `embed`) gebaut wurde oder das Modell fehlt.
pub const NOT_ENABLED: &str = "Der Dokumenten-Chatbot ist auf diesem Server nicht aktiviert.";
/// Meldung bei nicht erreichbarem Sprachmodell.
pub const LLM_UNAVAILABLE: &str =
    "Das Sprachmodell ist derzeit nicht erreichbar. Bitte später erneut versuchen.";
/// Längste erlaubte Frage in Zeichen.
const MAX_QUESTION_CHARS: usize = 4000;
/// Zeitlimit eines LLM-Aufrufs (eine Runde; Antworten auf 16-GB-GPUs können dauern).
#[cfg_attr(not(feature = "embed"), allow(dead_code))]
pub const LLM_TIMEOUT: Duration = Duration::from_secs(180);
/// Zeitlimit für den Reranker (eine Suche mit ~24 Kandidaten).
#[cfg_attr(not(feature = "embed"), allow(dead_code))]
pub const RERANK_TIMEOUT: Duration = Duration::from_secs(20);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Objektsichere Sicht auf einen `ChatService`.
pub trait Chatbot: Send + Sync {
    fn answer<'a>(
        &'a self,
        question: &'a str,
        history: &'a [ChatTurn],
        sink: Sink<'a>,
    ) -> BoxFuture<'a, Result<ChatResponse, ChatError>>;
}

impl<E: Embedder, S: Search, R: Rerank, L: Llm> Chatbot for ChatService<E, S, R, L> {
    fn answer<'a>(
        &'a self,
        question: &'a str,
        history: &'a [ChatTurn],
        sink: Sink<'a>,
    ) -> BoxFuture<'a, Result<ChatResponse, ChatError>> {
        Box::pin(ChatService::answer(self, question, history, sink))
    }
}

/// Merkt sich, ob gerade Chats laufen. Hintergrundarbeit am Sprachmodell (EK-Scan) pausiert
/// dann, damit der Chat die GPUs für sich hat.
#[derive(Clone, Default)]
pub struct ChatActivity(Arc<ActivityInner>);

#[derive(Default)]
struct ActivityInner {
    running: AtomicUsize,
    last_end: Mutex<Option<Instant>>,
    started: Notify,
}

/// Solange er lebt, gilt ein Chat als laufend.
pub struct ActivityGuard(ChatActivity);

impl ChatActivity {
    pub fn begin(&self) -> ActivityGuard {
        self.0.running.fetch_add(1, Ordering::SeqCst);
        self.0.started.notify_waiters();
        ActivityGuard(self.clone())
    }

    /// Läuft ein Chat oder endete einer vor weniger als `quiet`?
    pub fn busy(&self, quiet: Duration) -> bool {
        self.0.running.load(Ordering::SeqCst) > 0
            || self
                .0
                .last_end
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some_and(|t| t.elapsed() < quiet)
    }

    /// Wartet, bis ein neuer Chat beginnt.
    pub async fn started(&self) {
        self.0.started.notified().await;
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        *self.0.0.last_end.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
        self.0.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

pub type SharedChatbot = Arc<dyn Chatbot>;

/// Eine Chat-Anfrage für das Admin-Panel (ohne Fragetext).
#[derive(Debug, Clone)]
pub struct ChatEvent {
    pub at: DateTime<Utc>,
    pub user: String,
    pub millis: u128,
    pub ok: bool,
    /// z. B. „Antwort, 5 Quellen“, „nichts gefunden“ oder die volle Fehlerkette.
    pub outcome: String,
}

/// Die letzten Chat-Anfragen, nur im Speicher (nach einem Neustart leer).
#[derive(Clone, Default)]
pub struct ChatLog(Arc<Mutex<VecDeque<ChatEvent>>>);

impl ChatLog {
    const MAX: usize = 20;

    fn push(&self, e: ChatEvent) {
        let mut q = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if q.len() == Self::MAX {
            q.pop_back();
        }
        q.push_front(e);
    }

    /// Neueste zuerst.
    pub fn recent(&self) -> Vec<ChatEvent> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}

/// Bildet Fehler des Chat-Crates auf API-Fehler ab. Nicht erreichbares LLM ist 503
/// `llm_unavailable` (die LLM-VM kann ausgeschaltet sein), alles andere ein interner Fehler.
pub fn map_chat_error(err: &ChatError) -> ApiErr {
    match err {
        ChatError::Llm(LlmError::Http(_)) => ApiErr::llm_unavailable(LLM_UNAVAILABLE),
        ChatError::Llm(LlmError::Status { status, .. }) if *status >= 500 => {
            ApiErr::llm_unavailable(LLM_UNAVAILABLE)
        }
        other => {
            tracing::error!(error = %error_chain(other), "Chat fehlgeschlagen");
            ApiErr::internal()
        }
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

/// Prüft die Anfrage, bevor das Modell gefragt wird. Ohne Chatbot: 501.
pub fn check<'a>(
    bot: Option<&'a dyn Chatbot>,
    req: &'a ChatRequest,
) -> Result<(&'a dyn Chatbot, &'a str), ApiErr> {
    let Some(bot) = bot else {
        return Err(ApiErr::not_implemented(NOT_ENABLED));
    };
    let question = req.question.trim();
    if question.is_empty() {
        return Err(ApiErr::bad_request("Die Frage darf nicht leer sein"));
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(ApiErr::bad_request(format!(
            "Die Frage ist zu lang (höchstens {MAX_QUESTION_CHARS} Zeichen)"
        )));
    }
    Ok((bot, question))
}

/// Prüft die Anfrage und beantwortet sie. Statusmeldungen und Antwort-Stücke gehen an `sink`.
/// Jede Anfrage an das Modell landet in `log` (mit der vollen Fehlerursache, die die App nicht
/// sieht); solange sie läuft, gilt `activity` als belegt.
pub async fn run(
    bot: Option<&dyn Chatbot>,
    req: &ChatRequest,
    log: &ChatLog,
    user: &str,
    activity: &ChatActivity,
    sink: Sink<'_>,
) -> Result<ChatResponse, ApiErr> {
    let (bot, question) = check(bot, req)?;
    let started = Instant::now();
    let result = {
        let _busy = activity.begin();
        bot.answer(question, &req.history, sink).await
    };
    let (ok, outcome) = match &result {
        Ok(r) if r.sources.is_empty() => (true, "nichts gefunden".to_string()),
        Ok(r) => (true, format!("Antwort, {} Quellen", r.sources.len())),
        Err(e) => (false, error_chain(e)),
    };
    log.push(ChatEvent {
        at: Utc::now(),
        user: user.to_string(),
        millis: started.elapsed().as_millis(),
        ok,
        outcome,
    });
    result.map_err(|e| map_chat_error(&e))
}

/// Beantwortet die (mit [`check`] geprüfte) Anfrage im Hintergrund und liefert die Ereignisse
/// als NDJSON-Body, zuletzt `done` oder `error`. Wird der Body verworfen (die App bricht ab),
/// endet auch die Anfrage ans Modell.
pub fn stream(
    bot: SharedChatbot,
    req: ChatRequest,
    log: ChatLog,
    user: String,
    activity: ChatActivity,
) -> Body {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatStreamEvent>();
    tokio::spawn(async move {
        let sink = {
            let tx = tx.clone();
            move |e| {
                let _ = tx.send(e);
            }
        };
        let answer = run(Some(&*bot), &req, &log, &user, &activity, &sink);
        let last = tokio::select! {
            r = answer => match r {
                Ok(response) => ChatStreamEvent::Done { response },
                Err(e) => ChatStreamEvent::Error { error: e.body() },
            },
            () = tx.closed() => return,
        };
        let _ = tx.send(last);
    });
    Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        let event = rx.recv().await?;
        let mut line = serde_json::to_vec(&event).unwrap_or_default();
        line.push(b'\n');
        Some((Ok::<_, std::convert::Infallible>(Bytes::from(line)), rx))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use llm::{ChatMessage, ChatOptions, LlmClient};
    use retrieval::RetrievalError;

    struct Fake(Result<ChatResponse, fn() -> ChatError>);

    impl Chatbot for Fake {
        fn answer<'a>(
            &'a self,
            _q: &'a str,
            _h: &'a [ChatTurn],
            sink: Sink<'a>,
        ) -> BoxFuture<'a, Result<ChatResponse, ChatError>> {
            sink(ChatStreamEvent::Status {
                text: "Suche: x".into(),
            });
            let r = match &self.0 {
                Ok(r) => Ok(r.clone()),
                Err(f) => Err(f()),
            };
            Box::pin(async move { r })
        }
    }

    fn req(q: &str) -> ChatRequest {
        ChatRequest {
            question: q.into(),
            history: Vec::new(),
        }
    }

    #[test]
    fn llm_status_5xx_is_503_llm_unavailable() {
        let e = ChatError::Llm(LlmError::Status {
            status: 503,
            body: "loading".into(),
        });
        let api = map_chat_error(&e);
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.code, "llm_unavailable");
        assert_eq!(api.message, LLM_UNAVAILABLE);
    }

    #[test]
    fn other_errors_are_internal() {
        for e in [
            ChatError::Llm(LlmError::Status {
                status: 400,
                body: "x".into(),
            }),
            ChatError::Llm(LlmError::InvalidResponse("x".into())),
            ChatError::Retrieval(RetrievalError::Embed("x".into())),
        ] {
            let api = map_chat_error(&e);
            assert_eq!(api.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(api.code, "internal");
        }
    }

    #[tokio::test]
    async fn unreachable_llm_server_is_503() {
        // Port 1 ist geschlossen: Verbindungsfehler wie bei ausgeschalteter LLM-VM.
        let client = LlmClient::new("http://127.0.0.1:1", Duration::from_secs(5)).unwrap();
        let err = client
            .chat(&[ChatMessage::user("Hallo")], &ChatOptions::default())
            .await
            .unwrap_err();
        let api = map_chat_error(&ChatError::Llm(err));
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.code, "llm_unavailable");
    }

    async fn ask(
        bot: Option<&dyn Chatbot>,
        q: &str,
        log: &ChatLog,
        user: &str,
    ) -> Result<ChatResponse, ApiErr> {
        run(bot, &req(q), log, user, &ChatActivity::default(), &|_| {}).await
    }

    #[tokio::test]
    async fn run_without_bot_is_501_with_message() {
        let e = ask(None, "Frage", &ChatLog::default(), "t")
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(e.message, NOT_ENABLED);
    }

    #[tokio::test]
    async fn run_validates_question_and_passes_answer_through() {
        let ok = Fake(Ok(ChatResponse {
            answer: "Antwort".into(),
            sources: Vec::new(),
        }));
        let log = ChatLog::default();
        let e = ask(Some(&ok), "   ", &log, "t").await.unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        let e = ask(Some(&ok), &"a".repeat(MAX_QUESTION_CHARS + 1), &log, "t")
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            ask(Some(&ok), "Wo?", &log, "t").await.unwrap().answer,
            "Antwort"
        );

        let down = Fake(Err(|| {
            ChatError::Llm(LlmError::Status {
                status: 502,
                body: String::new(),
            })
        }));
        let e = ask(Some(&down), "Wo?", &log, "anna").await.unwrap_err();
        assert_eq!(e.code, "llm_unavailable");

        // Nur echte Modellaufrufe, neueste zuerst, mit voller Fehlerursache.
        let events = log.recent();
        assert_eq!(events.len(), 2);
        assert!(!events[0].ok && events[0].user == "anna" && events[0].outcome.contains("502"));
        assert!(events[1].ok && events[1].outcome == "nichts gefunden");
    }

    async fn stream_lines(bot: Fake) -> Vec<ChatStreamEvent> {
        let body = stream(
            Arc::new(bot),
            req("Wo?"),
            ChatLog::default(),
            "t".into(),
            ChatActivity::default(),
        );
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn stream_sends_status_then_done_or_error() {
        let ok = Fake(Ok(ChatResponse {
            answer: "Antwort".into(),
            sources: Vec::new(),
        }));
        let events = stream_lines(ok).await;
        assert_eq!(
            events[0],
            ChatStreamEvent::Status {
                text: "Suche: x".into()
            }
        );
        assert!(
            matches!(&events[1], ChatStreamEvent::Done { response } if response.answer == "Antwort")
        );
        assert_eq!(events.len(), 2);

        let down = Fake(Err(|| {
            ChatError::Llm(LlmError::InvalidResponse("x".into()))
        }));
        let events = stream_lines(down).await;
        assert!(matches!(&events[1], ChatStreamEvent::Error { error } if error.code == "internal"));
    }

    #[test]
    fn activity_is_busy_while_running_and_shortly_after() {
        let a = ChatActivity::default();
        assert!(!a.busy(Duration::from_secs(60)));
        let g = a.begin();
        assert!(a.busy(Duration::ZERO));
        drop(g);
        assert!(a.busy(Duration::from_secs(60)), "kurz nach dem Chat");
        assert!(!a.busy(Duration::ZERO));
    }
}
