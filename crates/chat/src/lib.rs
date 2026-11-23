//! Dokumenten-Chatbot: Frage einbetten, Hybrid-Suche, Antwort des LLM nur aus den Fundstellen.
//!
//! [`ChatService`] ist über Embedder, Suche und LLM generisch ([`Embedder`], [`Search`], [`Llm`]),
//! damit er ohne Datenbank und Modell testbar ist. Für den Betrieb: [`PgChatService`].

use std::future::Future;

use api_types::client::{ChatResponse, ChatSource, ChatTurn};
use llm::{ChatMessage, ChatOptions, LlmClient};
use retrieval::{Embedder, Hit, PgVectorStore};

/// Antwort, wenn die Suche nichts Passendes liefert.
pub const NOT_FOUND: &str = "Dazu habe ich in den Dokumenten nichts gefunden.";

/// Anzahl Fundstellen, die ins Prompt und in die Quellenliste gehen.
pub const TOP_K: usize = 8;
/// Kandidaten je Suchverfahren vor der Fusion.
pub const CANDIDATES: i64 = 30;
/// Längste Quellenvorschau in der Antwort.
pub const SNIPPET_CHARS: usize = 300;
/// Pro Fundstelle höchstens so viel Text im Prompt.
const PROMPT_CHARS_PER_SOURCE: usize = 2500;
/// Wie viele frühere Runden mitgegeben werden.
const MAX_HISTORY_TURNS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("Suche fehlgeschlagen")]
    Retrieval(#[from] retrieval::RetrievalError),
    #[error("LLM-Aufruf fehlgeschlagen")]
    Llm(#[from] llm::LlmError),
}

/// Hybrid-Suche über die indizierten Chunks.
pub trait Search: Send + Sync {
    fn search(
        &self,
        query: &str,
        embedding: Vec<f32>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Hit>, ChatError>> + Send;
}

/// Erzeugt die Antwort aus fertig aufgebauten Nachrichten.
pub trait Llm: Send + Sync {
    fn complete(
        &self,
        messages: Vec<ChatMessage>,
    ) -> impl Future<Output = Result<String, ChatError>> + Send;
}

impl Search for PgVectorStore {
    async fn search(
        &self,
        query: &str,
        embedding: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<Hit>, ChatError> {
        Ok(self
            .hybrid_search(query, embedding, CANDIDATES, limit)
            .await?)
    }
}

impl Llm for LlmClient {
    async fn complete(&self, messages: Vec<ChatMessage>) -> Result<String, ChatError> {
        let options = ChatOptions {
            temperature: 0.1,
            max_tokens: Some(1024),
        };
        Ok(self.chat(&messages, &options).await?)
    }
}

pub struct ChatService<E, S, L> {
    embedder: E,
    search: S,
    llm: L,
}

/// Betriebsvariante: lokaler Embedder (BGE-M3), pgvector-Suche, llama-server.
pub type PgChatService<E> = ChatService<E, PgVectorStore, LlmClient>;

impl<E: Embedder, S: Search, L: Llm> ChatService<E, S, L> {
    pub fn new(embedder: E, search: S, llm: L) -> Self {
        Self {
            embedder,
            search,
            llm,
        }
    }

    /// Beantwortet `question` aus den Dokumenten. `history`: frühere Runden, älteste zuerst.
    ///
    /// Das Einbetten der Frage blockiert kurz (Modellinferenz); in einem Async-Server bei Bedarf
    /// den Aufruf in `spawn_blocking` legen oder einen Embedder mit eigener Warteschlange nutzen.
    pub async fn answer(
        &self,
        question: &str,
        history: &[ChatTurn],
    ) -> Result<ChatResponse, ChatError> {
        // Rückfragen („und 2020?“) allein finden nichts; die vorige Frage gibt den Zusammenhang.
        let query = match history.last() {
            Some(t) => format!("{} {question}", t.question),
            None => question.to_string(),
        };
        let embeddings = self.embedder.embed(std::slice::from_ref(&query))?;
        let embedding = embeddings.into_iter().next().unwrap_or_default();
        let hits = self.search.search(&query, embedding, TOP_K).await?;
        if hits.is_empty() {
            return Ok(ChatResponse {
                answer: NOT_FOUND.to_string(),
                sources: Vec::new(),
            });
        }

        // Passt der Prompt nicht in den Kontext des Sprachmodells (lange OCR-Texte, langer
        // Verlauf), erst die Quellen halbieren, zuletzt ohne Verlauf fragen.
        let mut k = hits.len();
        let mut hist = history;
        let answer = loop {
            let messages = build_messages(question, hist, &hits[..k]);
            match self.llm.complete(messages).await {
                Err(e) if is_context_overflow(&e) && k > 1 => k /= 2,
                Err(e) if is_context_overflow(&e) && !hist.is_empty() => hist = &[],
                r => break r?.trim().to_string(),
            }
            tracing::info!(
                quellen = k,
                verlauf = hist.len(),
                "Prompt zu lang, neuer Versuch"
            );
        };
        let hits = &hits[..k];
        if answer.is_empty() || answer.starts_with(NOT_FOUND) {
            return Ok(ChatResponse {
                answer: NOT_FOUND.to_string(),
                sources: Vec::new(),
            });
        }
        Ok(ChatResponse {
            answer,
            sources: hits.iter().map(to_source).collect(),
        })
    }
}

pub const SYSTEM_PROMPT: &str = "Du bist der Dokumenten-Assistent eines GaLaBau-Betriebs. Du bekommst nummerierte Auszüge aus den Firmendokumenten (Angebote, Leistungsverzeichnisse, Rechnungen, Schriftverkehr, Bautagebücher). Viele sind eingescannt und per Texterkennung gelesen, also nicht fehlerfrei. Der Pfad vor jedem Auszug nennt meist Jahr, Auftraggeber und Projekt. Beantworte die Frage konkret aus diesen Auszügen: Nenne Preise, Mengen, Daten, Firmen und Projekte genau so, wie sie dort stehen, und setze hinter jede Aussage die Quelle als [1], [2] usw. Passen mehrere Projekte oder Jahre, liste sie einzeln auf. Beantworten die Auszüge die Frage nur teilweise, sage, was du gefunden hast und was fehlt. Nur wenn kein Auszug zur Frage passt, antworte genau mit: \"Dazu habe ich in den Dokumenten nichts gefunden.\" Erfinde nichts und nutze kein Wissen außerhalb der Auszüge. Antworte auf Deutsch.";

/// llama-server lehnt Anfragen über der Kontextgröße mit 400 ab.
fn is_context_overflow(e: &ChatError) -> bool {
    matches!(e, ChatError::Llm(llm::LlmError::Status { status: 400, body })
        if body.contains("context size"))
}

/// System-Prompt, frühere Runden und die Frage samt nummerierten Quellen.
pub fn build_messages(question: &str, history: &[ChatTurn], hits: &[Hit]) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage::system(SYSTEM_PROMPT)];
    let skip = history.len().saturating_sub(MAX_HISTORY_TURNS);
    for turn in &history[skip..] {
        messages.push(ChatMessage::user(turn.question.clone()));
        messages.push(ChatMessage::assistant(turn.answer.clone()));
    }

    let mut user = String::from("Quellen:\n");
    for (i, hit) in hits.iter().enumerate() {
        let place = match hit.page {
            Some(p) => format!("{}, Seite {p}", hit.rel_path),
            None => hit.rel_path.clone(),
        };
        user.push_str(&format!(
            "\n[{}] {place}\n{}\n",
            i + 1,
            truncate_chars(&clean(&hit.text), PROMPT_CHARS_PER_SOURCE)
        ));
    }
    user.push_str(&format!("\nFrage: {question}"));
    messages.push(ChatMessage::user(user));
    messages
}

/// Füllpunkte („........“) und Leerraum-Läufe aus OCR/LV-Text kosten nur Kontext.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut leader = String::new();
    let flush = |out: &mut String, leader: &mut String| {
        if leader.chars().count() >= 3 {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str("… ");
        } else {
            out.push_str(leader);
        }
        leader.clear();
    };
    for c in text.trim().chars() {
        if matches!(c, '.' | '_' | '…') {
            leader.push(c);
            continue;
        }
        flush(&mut out, &mut leader);
        if c == ' ' || c == '\t' {
            if !out.ends_with([' ', '\n']) {
                out.push(' ');
            }
        } else if c == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    flush(&mut out, &mut leader);
    out.trim_end().to_owned()
}

fn to_source(hit: &Hit) -> ChatSource {
    ChatSource {
        path: hit.rel_path.clone(),
        page: hit.page,
        snippet: snippet(&hit.text),
        score: hit.score,
    }
}

/// Vorschau: Leerraum zusammengezogen, höchstens [`SNIPPET_CHARS`] Zeichen (inkl. „…“).
pub fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&flat, SNIPPET_CHARS)
}

/// Kürzt auf `max` Zeichen (Zeichen, nicht Bytes); gekürzte Texte enden auf „…“ und bleiben `<= max`.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use retrieval::HashEmbedder;
    use std::sync::Mutex;
    use uuid::Uuid;

    struct FakeSearch(Vec<Hit>);
    impl Search for FakeSearch {
        async fn search(
            &self,
            _q: &str,
            emb: Vec<f32>,
            limit: usize,
        ) -> Result<Vec<Hit>, ChatError> {
            assert_eq!(emb.len(), 32, "Frage wurde nicht eingebettet");
            Ok(self.0.iter().take(limit).cloned().collect())
        }
    }

    struct FakeLlm {
        reply: String,
        seen: Mutex<Vec<ChatMessage>>,
        /// Wie llama-server: längere Prompts (in Zeichen) werden mit 400 abgelehnt.
        max_chars: usize,
    }
    impl Llm for FakeLlm {
        async fn complete(&self, messages: Vec<ChatMessage>) -> Result<String, ChatError> {
            let len: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            *self.seen.lock().unwrap() = messages;
            if len > self.max_chars {
                return Err(ChatError::Llm(llm::LlmError::Status {
                    status: 400,
                    body: format!("request ({len} tokens) exceeds the available context size"),
                }));
            }
            Ok(self.reply.clone())
        }
    }

    fn hit(path: &str, page: Option<u32>, text: &str) -> Hit {
        Hit {
            chunk_id: Uuid::nil(),
            rel_path: path.into(),
            page,
            text: text.into(),
            score: 0.03,
        }
    }

    fn service(hits: Vec<Hit>, reply: &str) -> ChatService<HashEmbedder, FakeSearch, FakeLlm> {
        ChatService::new(
            HashEmbedder::new(32),
            FakeSearch(hits),
            FakeLlm {
                reply: reply.into(),
                seen: Mutex::new(Vec::new()),
                max_chars: usize::MAX,
            },
        )
    }

    #[tokio::test]
    async fn answers_with_numbered_sources_and_short_snippets() {
        let long = "Pflaster ".repeat(100);
        let svc = service(
            vec![
                hit("Angebote/2019/Kita.pdf", Some(3), &long),
                hit("Preise.xlsx", None, "Kies 12 EUR"),
            ],
            "Das Pflaster kostet 12 EUR [2].",
        );
        let history = [ChatTurn {
            question: "Hallo?".into(),
            answer: "Hallo.".into(),
        }];
        let resp = svc.answer("Was kostet Kies?", &history).await.unwrap();

        assert_eq!(resp.answer, "Das Pflaster kostet 12 EUR [2].");
        assert_eq!(resp.sources.len(), 2);
        assert_eq!(resp.sources[0].path, "Angebote/2019/Kita.pdf");
        assert_eq!(resp.sources[0].page, Some(3));
        assert!(resp.sources[0].snippet.chars().count() <= SNIPPET_CHARS);
        assert!(resp.sources[0].snippet.ends_with('…'));
        assert_eq!(resp.sources[1].page, None);
        assert_eq!(resp.sources[1].snippet, "Kies 12 EUR");

        let seen = svc.llm.seen.lock().unwrap();
        assert_eq!(seen[0].role, llm::Role::System);
        assert_eq!(seen[1].content, "Hallo?");
        assert_eq!(seen[2].content, "Hallo.");
        let last = &seen.last().unwrap().content;
        assert!(last.contains("[1] Angebote/2019/Kita.pdf, Seite 3"));
        assert!(last.contains("[2] Preise.xlsx\nKies 12 EUR"));
        assert!(last.ends_with("Frage: Was kostet Kies?"));
    }

    #[tokio::test]
    async fn no_hits_means_not_found_without_llm_call() {
        let svc = service(vec![], "darf nie kommen");
        let resp = svc.answer("Irgendwas?", &[]).await.unwrap();
        assert_eq!(resp.answer, NOT_FOUND);
        assert!(resp.sources.is_empty());
        assert!(svc.llm.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn llm_not_found_drops_sources() {
        let svc = service(
            vec![hit("a.pdf", Some(1), "irrelevant")],
            "Dazu habe ich in den Dokumenten nichts gefunden.",
        );
        let resp = svc.answer("Frage", &[]).await.unwrap();
        assert_eq!(resp.answer, NOT_FOUND);
        assert!(resp.sources.is_empty());
    }

    #[test]
    fn history_is_capped() {
        let history: Vec<ChatTurn> = (0..10)
            .map(|i| ChatTurn {
                question: format!("f{i}"),
                answer: format!("a{i}"),
            })
            .collect();
        let msgs = build_messages("jetzt", &history, &[hit("a.pdf", None, "x")]);
        // System + 4 Runden à 2 + Frage
        assert_eq!(msgs.len(), 1 + 8 + 1);
        assert_eq!(msgs[1].content, "f6");
    }

    #[test]
    fn clean_drops_leaders_and_blank_runs() {
        assert_eq!(
            clean("Pos 1.2   Pflaster ........ 12,50 €\n\n\n\nSumme ___ z.B. a_b Ende..."),
            "Pos 1.2 Pflaster … 12,50 €\n\nSumme … z.B. a_b Ende …"
        );
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "ä".repeat(400);
        let t = snippet(&s);
        assert_eq!(t.chars().count(), SNIPPET_CHARS);
        assert!(t.ends_with('…'));
        assert_eq!(snippet("kurz\n\n  text"), "kurz text");
    }

    #[tokio::test]
    async fn too_long_prompt_retries_with_fewer_sources_then_without_history() {
        let hits: Vec<Hit> = (0..8)
            .map(|i| hit(&format!("d{i}.pdf"), None, &"x".repeat(1000)))
            .collect();
        let mut svc = service(hits, "Antwort [1]");
        let history = vec![ChatTurn {
            question: "alt".into(),
            answer: "y".repeat(3000),
        }];

        // 2 Quellen passen nur ohne Verlauf nicht: erst 8 -> 4 -> 2 -> 1, dann ohne Verlauf.
        svc.llm.max_chars = 2000;
        let resp = svc.answer("Frage", &history).await.unwrap();
        assert_eq!(resp.answer, "Antwort [1]");
        assert_eq!(resp.sources.len(), 1);
        assert_eq!(
            svc.llm.seen.lock().unwrap().len(),
            2,
            "System + Frage, kein Verlauf"
        );

        // Passt gar nichts, kommt der Fehler durch.
        svc.llm.max_chars = 10;
        assert!(svc.answer("Frage", &history).await.is_err());
    }
}
