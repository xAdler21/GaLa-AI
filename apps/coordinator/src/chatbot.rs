//! Dokumenten-Chatbot im Coordinator: Anbindung von `chat::ChatService` und Fehlerabbildung.
//!
//! `ChatService` ist über Embedder/Suche/LLM generisch (nicht objektsicher); hier wird er hinter
//! [`Chatbot`] versteckt, damit `AppState` ihn ohne Typparameter halten und Tests ihn ersetzen können.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use api_types::client::{ChatRequest, ChatResponse, ChatTurn};
use chat::{ChatError, ChatService, Llm, Search};
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
/// Zeitlimit eines LLM-Aufrufs (Antworten auf 16-GB-GPUs können dauern).
#[cfg_attr(not(feature = "embed"), allow(dead_code))]
pub const LLM_TIMEOUT: Duration = Duration::from_secs(180);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Objektsichere Sicht auf einen `ChatService`.
pub trait Chatbot: Send + Sync {
    fn answer<'a>(
        &'a self,
        question: &'a str,
        history: &'a [ChatTurn],
    ) -> BoxFuture<'a, Result<ChatResponse, ChatError>>;
}

impl<E: Embedder, S: Search, L: Llm> Chatbot for ChatService<E, S, L> {
    fn answer<'a>(
        &'a self,
        question: &'a str,
        history: &'a [ChatTurn],
    ) -> BoxFuture<'a, Result<ChatResponse, ChatError>> {
        Box::pin(ChatService::answer(self, question, history))
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

/// Prüft die Anfrage und beantwortet sie. Ohne Chatbot: 501. Jede Anfrage an das Modell
/// landet in `log` (mit der vollen Fehlerursache, die die App nicht sieht).
pub async fn run(
    bot: Option<&dyn Chatbot>,
    req: &ChatRequest,
    log: &ChatLog,
    user: &str,
) -> Result<ChatResponse, ApiErr> {
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
    let started = Instant::now();
    let result = bot.answer(question, &req.history).await;
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
        ) -> BoxFuture<'a, Result<ChatResponse, ChatError>> {
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

    #[tokio::test]
    async fn run_without_bot_is_501_with_message() {
        let e = run(None, &req("Frage"), &ChatLog::default(), "t")
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
        let e = run(Some(&ok), &req("   "), &log, "t").await.unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        let e = run(
            Some(&ok),
            &req(&"a".repeat(MAX_QUESTION_CHARS + 1)),
            &log,
            "t",
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            run(Some(&ok), &req("Wo?"), &log, "t").await.unwrap().answer,
            "Antwort"
        );

        let down = Fake(Err(|| {
            ChatError::Llm(LlmError::Status {
                status: 502,
                body: String::new(),
            })
        }));
        let e = run(Some(&down), &req("Wo?"), &log, "anna")
            .await
            .unwrap_err();
        assert_eq!(e.code, "llm_unavailable");

        // Nur echte Modellaufrufe, neueste zuerst, mit voller Fehlerursache.
        let events = log.recent();
        assert_eq!(events.len(), 2);
        assert!(!events[0].ok && events[0].user == "anna" && events[0].outcome.contains("502"));
        assert!(events[1].ok && events[1].outcome == "nichts gefunden");
    }
}
