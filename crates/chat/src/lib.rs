//! Dokumenten-Chatbot: Das Sprachmodell sucht selbst (Werkzeuge `suchen`, `seite_lesen`,
//! `ordner_auflisten`) und antwortet nur aus den gefundenen, nummerierten Quellen.
//!
//! Ablauf je Frage ([`ChatService::answer`]):
//! 1. Die erste Runde erzwingt ein Werkzeug: Das Modell formuliert die Suchanfrage selbst
//!    (Rückfragen aufgelöst, Synonyme, Jahr, Kunde).
//! 2. Jede Suche: Hybrid-Suche (BGE-M3 + Volltext), danach sortiert der Reranker neu; die besten
//!    [`AGENT_HITS`] gehen nummeriert ans Modell. Die Nummern gelten für die ganze Frage.
//! 3. Höchstens [`MAX_ROUNDS`] Runden; in der letzten sind keine Werkzeuge mehr erlaubt.
//! 4. Ruft das Modell gar kein Werkzeug auf oder läuft der Kontext über, gilt der einfache Weg:
//!    eine Suche mit der Frage und eine Antwort aus den Fundstellen ([`build_messages`]).
//!
//! Text-Stücke der Antwort und Statusmeldungen gehen sofort an die [`Sink`] (Streaming zur App).
//! [`ChatService`] ist über Embedder, Suche, Reranker und LLM generisch, damit er ohne Datenbank
//! und Modell testbar ist. Für den Betrieb: [`PgChatService`].

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use api_types::client::{ChatResponse, ChatSource, ChatStreamEvent, ChatTurn};
use llm::{ChatMessage, ChatOptions, LlmClient, ToolCall, Turn};
use retrieval::{Embedder, FolderEntry, Hit, PgVectorStore};
use serde_json::{Value, json};

/// Antwort, wenn die Suche nichts Passendes liefert.
pub const NOT_FOUND: &str = "Dazu habe ich in den Dokumenten nichts gefunden.";

/// Fundstellen im einfachen Weg (ohne Werkzeuge).
pub const TOP_K: usize = 8;
/// Fundstellen je Werkzeug-Suche (das Modell kann mehrmals suchen).
pub const AGENT_HITS: usize = 6;
/// Kandidaten je Suchverfahren vor der Fusion.
pub const CANDIDATES: i64 = 30;
/// So viele fusionierte Kandidaten bewertet der Reranker.
pub const RERANK_CANDIDATES: usize = 24;
/// Runden mit dem Modell je Frage, die letzte ohne Werkzeuge.
pub const MAX_ROUNDS: usize = 4;
/// Längste Quellenvorschau in der Antwort.
pub const SNIPPET_CHARS: usize = 300;
/// Pro Fundstelle höchstens so viel Text im Prompt (einfacher Weg).
const PROMPT_CHARS_PER_SOURCE: usize = 2500;
/// Pro Fundstelle höchstens so viel Text im Ergebnis von `suchen`.
const TOOL_CHARS_PER_HIT: usize = 1500;
/// Höchstens so viel Text liefert `seite_lesen`.
const PAGE_CHARS: usize = 6000;
/// Der Reranker liest je Kandidat nur den Anfang (Rechenzeit).
const RERANK_DOC_CHARS: usize = 1200;
/// Höchstens so viele Einträge liefert `ordner_auflisten`.
const FOLDER_LIMIT: i64 = 80;
/// Mehr Werkzeugaufrufe je Runde werden nicht ausgeführt (Kontext).
const MAX_CALLS_PER_ROUND: usize = 3;
/// Wie viele frühere Runden mitgegeben werden.
const MAX_HISTORY_TURNS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("Suche fehlgeschlagen")]
    Retrieval(#[from] retrieval::RetrievalError),
    #[error("LLM-Aufruf fehlgeschlagen")]
    Llm(#[from] llm::LlmError),
}

/// Empfänger für Statusmeldungen und Antwort-Stücke.
pub type Sink<'a> = &'a (dyn Fn(ChatStreamEvent) + Send + Sync);

/// Zugriff auf den Index: Hybrid-Suche, Seitentext, Ordner.
pub trait Search: Send + Sync {
    fn search(
        &self,
        query: &str,
        embedding: Vec<f32>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Hit>, ChatError>> + Send;

    fn page(
        &self,
        rel_path: &str,
        page: Option<u32>,
    ) -> impl Future<Output = Result<Option<Hit>, ChatError>> + Send;

    fn folder(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Vec<FolderEntry>, ChatError>> + Send;
}

/// Bewertet Kandidaten neu. `None`: kein Reranker (die Reihenfolge der Suche bleibt).
pub trait Rerank: Send + Sync {
    fn rerank(
        &self,
        query: &str,
        docs: Vec<String>,
    ) -> impl Future<Output = Option<Vec<f32>>> + Send;
}

/// Eine Runde mit dem Modell; Text-Stücke gehen an `on_delta`, `false` bricht ab.
pub trait Llm: Send + Sync {
    fn turn(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<(&Value, &str)>,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> impl Future<Output = Result<Turn, ChatError>> + Send;
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

    async fn page(&self, rel_path: &str, page: Option<u32>) -> Result<Option<Hit>, ChatError> {
        Ok(self.page_text(rel_path, page, PAGE_CHARS).await?)
    }

    async fn folder(&self, path: &str) -> Result<Vec<FolderEntry>, ChatError> {
        Ok(self.list_folder(path, FOLDER_LIMIT).await?)
    }
}

/// Reranker über einen eigenen llama-server (`--reranking`). Ist er nicht erreichbar, bleibt
/// die Reihenfolge der Suche (der Chat soll daran nicht scheitern), und er wird
/// [`RERANK_RETRY`] lang nicht mehr gefragt (jede Suche würde sonst auf das Zeitlimit warten).
pub struct LlmReranker {
    client: Option<LlmClient>,
    down_until: Mutex<Option<Instant>>,
}

pub const RERANK_RETRY: Duration = Duration::from_secs(300);

impl LlmReranker {
    /// `None`: ohne Reranker.
    pub fn new(client: Option<LlmClient>) -> Self {
        Self {
            client,
            down_until: Mutex::new(None),
        }
    }
}

impl Rerank for LlmReranker {
    async fn rerank(&self, query: &str, docs: Vec<String>) -> Option<Vec<f32>> {
        let client = self.client.as_ref()?;
        let paused = || self.down_until.lock().unwrap_or_else(|p| p.into_inner());
        if paused().is_some_and(|t| Instant::now() < t) {
            return None;
        }
        match client.rerank(query, &docs).await {
            Ok(scores) => Some(scores),
            Err(e) => {
                tracing::warn!(error = %e, "Reranker nicht erreichbar, Reihenfolge der Suche bleibt");
                *paused() = Some(Instant::now() + RERANK_RETRY);
                None
            }
        }
    }
}

impl Llm for LlmClient {
    async fn turn(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<(&Value, &str)>,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<Turn, ChatError> {
        let options = ChatOptions {
            temperature: 0.1,
            max_tokens: Some(1500),
            dry_multiplier: Some(0.8),
        };
        Ok(self
            .chat_stream(&messages, &options, tools, on_delta)
            .await?)
    }
}

pub struct ChatService<E, S, R, L> {
    embedder: E,
    search: S,
    reranker: R,
    llm: L,
}

/// Betriebsvariante: lokaler Embedder (BGE-M3), pgvector-Suche, Reranker und Sprachmodell
/// über llama-server.
pub type PgChatService<E> = ChatService<E, PgVectorStore, LlmReranker, LlmClient>;

/// Gefundene Quellen einer Frage; Nummer = Position + 1.
#[derive(Default)]
struct Sources(Vec<Hit>);

impl Sources {
    /// Nummer der Quelle und ob sie neu ist. Gleiche Chunks bekommen dieselbe Nummer.
    fn add(&mut self, hit: Hit, dedupe: bool) -> (usize, bool) {
        if dedupe && let Some(i) = self.0.iter().position(|h| h.chunk_id == hit.chunk_id) {
            return (i + 1, false);
        }
        self.0.push(hit);
        (self.0.len(), true)
    }
}

impl<E: Embedder, S: Search, R: Rerank, L: Llm> ChatService<E, S, R, L> {
    pub fn new(embedder: E, search: S, reranker: R, llm: L) -> Self {
        Self {
            embedder,
            search,
            reranker,
            llm,
        }
    }

    /// Beantwortet `question` aus den Dokumenten. `history`: frühere Runden, älteste zuerst.
    ///
    /// Das Einbetten der Suchanfragen blockiert kurz (Modellinferenz auf der CPU).
    pub async fn answer(
        &self,
        question: &str,
        history: &[ChatTurn],
        sink: Sink<'_>,
    ) -> Result<ChatResponse, ChatError> {
        let tools = tools();
        let mut sources = Sources::default();
        let mut messages = agent_messages(question, history);
        let mut answer = None;
        for round in 0..MAX_ROUNDS {
            let choice = match round {
                0 => "required",
                r if r + 1 == MAX_ROUNDS => "none",
                _ => "auto",
            };
            let mut streamed = false;
            let result = {
                let mut text = String::new();
                let mut on_delta = |d: &str| {
                    streamed = true;
                    sink(ChatStreamEvent::Delta { text: d.to_owned() });
                    text.push_str(d);
                    !repeats(&text)
                };
                self.llm
                    .turn(messages.clone(), Some((&tools, choice)), &mut on_delta)
                    .await
            };
            if streamed && !matches!(&result, Ok(t) if t.tool_calls.is_empty() && round > 0) {
                sink(ChatStreamEvent::Reset);
            }
            let turn = match result {
                Err(e) if is_context_overflow(&e) => {
                    tracing::info!(runde = round, "Kontext voll, Antwort auf einfachem Weg");
                    break;
                }
                r => r?,
            };
            if turn.tool_calls.is_empty() {
                // In Runde 1 ohne Suche geantwortet: nicht verwenden, einfacher Weg.
                if round > 0 {
                    answer = Some(turn.content);
                }
                break;
            }
            messages.push(ChatMessage::assistant_tools(
                turn.content,
                turn.tool_calls.clone(),
            ));
            for (i, call) in turn.tool_calls.iter().enumerate() {
                let result = if i < MAX_CALLS_PER_ROUND {
                    self.run_tool(call, &mut sources, sink).await?
                } else {
                    format!("Übersprungen: höchstens {MAX_CALLS_PER_ROUND} Werkzeuge je Runde.")
                };
                messages.push(ChatMessage::tool(&call.id, result));
            }
        }
        let answer = match answer {
            Some(a) => a,
            None => self.classic(question, history, &mut sources, sink).await?,
        };
        Ok(finish(&answer, &sources))
    }

    /// Hybrid-Suche, danach Reranker; die besten `limit` Treffer.
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<Hit>, ChatError> {
        let embeddings = self.embedder.embed(&[query.to_owned()])?;
        let embedding = embeddings.into_iter().next().unwrap_or_default();
        let mut hits = self
            .search
            .search(query, embedding, RERANK_CANDIDATES)
            .await?;
        // Mit Pfad: Kunde, Ort und Projekt stehen oft nur dort.
        let docs = hits
            .iter()
            .map(|h| {
                format!(
                    "{}\n{}",
                    place(h),
                    truncate_chars(&clean(&h.text), RERANK_DOC_CHARS)
                )
            })
            .collect();
        if let Some(scores) = self.reranker.rerank(query, docs).await
            && scores.len() == hits.len()
        {
            let mut scored: Vec<(f32, Hit)> = scores.into_iter().zip(hits).collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            hits = scored
                .into_iter()
                .map(|(s, mut h)| {
                    h.score = s;
                    h
                })
                .collect();
        }
        hits.truncate(limit);
        Ok(hits)
    }

    async fn run_tool(
        &self,
        call: &ToolCall,
        sources: &mut Sources,
        sink: Sink<'_>,
    ) -> Result<String, ChatError> {
        let args: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
        let text = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        match call.function.name.as_str() {
            "suchen" => {
                let query = text("anfrage");
                if query.is_empty() {
                    return Ok("Fehler: Die Suchanfrage (`anfrage`) fehlt.".into());
                }
                status(sink, format!("Suche: {query}"));
                let hits = self.retrieve(&query, AGENT_HITS).await?;
                if hits.is_empty() {
                    return Ok("Keine Treffer. Versuche andere Begriffe.".into());
                }
                Ok(format_hits(hits, sources, TOOL_CHARS_PER_HIT, true))
            }
            "seite_lesen" => {
                let path = text("pfad");
                if path.is_empty() {
                    return Ok("Fehler: Der Pfad (`pfad`) fehlt.".into());
                }
                // Modelle schicken Zahlen manchmal als Text.
                let page = args.get("seite").and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str()?.trim().parse().ok())
                        .and_then(|p| u32::try_from(p).ok())
                });
                status(
                    sink,
                    match page {
                        Some(p) => format!("Lese {path}, Seite {p}"),
                        None => format!("Lese {path}"),
                    },
                );
                Ok(match self.search.page(&path, page).await? {
                    Some(hit) => format_hits(vec![hit], sources, PAGE_CHARS, false),
                    None => "Nicht gefunden: Diesen Pfad oder diese Seite gibt es im Index nicht."
                        .into(),
                })
            }
            "ordner_auflisten" => {
                let path = text("pfad");
                status(
                    sink,
                    if path.is_empty() {
                        "Öffne das Archiv".to_owned()
                    } else {
                        format!("Öffne Ordner {path}")
                    },
                );
                let entries = self.search.folder(&path).await?;
                Ok(format_folder(&path, &entries))
            }
            other => Ok(format!("Unbekanntes Werkzeug: {other}")),
        }
    }

    /// Einfacher Weg: Antwort aus den bisherigen Quellen oder aus einer Suche mit der Frage.
    /// Passt der Prompt nicht in den Kontext, erst die Quellen halbieren, zuletzt ohne Verlauf.
    async fn classic(
        &self,
        question: &str,
        history: &[ChatTurn],
        sources: &mut Sources,
        sink: Sink<'_>,
    ) -> Result<String, ChatError> {
        if sources.0.is_empty() {
            // Rückfragen („und 2020?“) allein finden nichts; die vorige Frage gibt den Zusammenhang.
            let query = match history.last() {
                Some(t) => format!("{} {question}", t.question),
                None => question.to_string(),
            };
            status(sink, format!("Suche: {query}"));
            for hit in self.retrieve(&query, TOP_K).await? {
                sources.add(hit, true);
            }
        }
        if sources.0.is_empty() {
            return Ok(NOT_FOUND.to_owned());
        }
        let mut k = sources.0.len().min(TOP_K);
        let mut hist = history;
        loop {
            let messages = build_messages(question, hist, &sources.0[..k]);
            let mut text = String::new();
            let mut on_delta = |d: &str| {
                sink(ChatStreamEvent::Delta { text: d.to_owned() });
                text.push_str(d);
                !repeats(&text)
            };
            match self.llm.turn(messages, None, &mut on_delta).await {
                Err(e) if is_context_overflow(&e) && k > 1 => k /= 2,
                Err(e) if is_context_overflow(&e) && !hist.is_empty() => hist = &[],
                r => {
                    sources.0.truncate(k);
                    return Ok(r?.content);
                }
            }
            tracing::info!(
                quellen = k,
                verlauf = hist.len(),
                "Prompt zu lang, neuer Versuch"
            );
        }
    }
}

fn status(sink: Sink<'_>, text: String) {
    sink(ChatStreamEvent::Status { text });
}

/// Kürzere Zeilen (Leerzeilen, „- “) dürfen sich wiederholen.
const MIN_LOOP_LINE: usize = 20;

/// Hängt das Modell in einer Schleife? Eine fertige, nicht kurze Zeile kommt zum dritten Mal.
fn repeats(text: &str) -> bool {
    let Some((done, _)) = text.rsplit_once('\n') else {
        return false;
    };
    let mut lines: Vec<&str> = done
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().count() >= MIN_LOOP_LINE)
        .collect();
    let Some(last) = lines.pop() else {
        return false;
    };
    lines.iter().filter(|l| **l == last).count() >= 2
}

/// Wiederholte Zeilen (Schleife des Modells) nur einmal; eine abgebrochene letzte Zeile, die
/// nur eine frühere wiederholt, fällt weg.
fn drop_repeated_lines(text: &str) -> String {
    let mut seen = BTreeSet::new();
    let mut out: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.chars().count() < MIN_LOOP_LINE || seen.insert(t)
        })
        .collect();
    if let Some((last, before)) = out.split_last()
        && !last.trim().is_empty()
        && before
            .iter()
            .any(|l| l.trim().len() > last.trim().len() && l.trim().starts_with(last.trim()))
    {
        out.pop();
    }
    out.join("\n")
}

/// Fertige Antwort mit den zitierten Quellen. Zitiert das Modell nichts, gehen alle mit.
fn finish(answer: &str, sources: &Sources) -> ChatResponse {
    let answer = drop_repeated_lines(answer.trim());
    let answer = answer.trim();
    if answer.is_empty() || answer.starts_with(NOT_FOUND) {
        return ChatResponse {
            answer: NOT_FOUND.to_string(),
            sources: Vec::new(),
        };
    }
    let cited = cited_numbers(answer);
    let all = sources
        .0
        .iter()
        .enumerate()
        .map(|(i, h)| to_source(i + 1, h));
    let picked: Vec<ChatSource> = all.clone().filter(|s| cited.contains(&s.nr)).collect();
    ChatResponse {
        answer: answer.to_string(),
        sources: if picked.is_empty() {
            all.take(TOP_K).collect()
        } else {
            picked
        },
    }
}

/// Nummern in Klammern wie `[2]`, `[1, 3]` oder `[2-4]`.
pub fn cited_numbers(text: &str) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for part in text.split('[').skip(1) {
        let Some((inner, _)) = part.split_once(']') else {
            continue;
        };
        if inner.trim().is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ',' | ' ' | '-' | '–'))
        {
            continue;
        }
        for group in inner.split(',') {
            let ends: Vec<u32> = group
                .split(['-', '–'])
                .filter_map(|n| n.trim().parse().ok())
                .collect();
            match ends[..] {
                [a] => {
                    out.insert(a);
                }
                [a, b] if a <= b && b - a < 20 => out.extend(a..=b),
                _ => {}
            }
        }
    }
    out
}

fn place(hit: &Hit) -> String {
    match hit.page {
        Some(p) => format!("{}, Seite {p}", hit.rel_path),
        None => hit.rel_path.clone(),
    }
}

/// Ergebnis eines Werkzeugs: nummerierte Fundstellen; schon bekannte nur als Verweis.
fn format_hits(hits: Vec<Hit>, sources: &mut Sources, max_chars: usize, dedupe: bool) -> String {
    let mut out = String::new();
    for hit in hits {
        let place = place(&hit);
        let text = truncate_chars(&clean(&hit.text), max_chars);
        match sources.add(hit, dedupe) {
            (nr, true) => out.push_str(&format!("[{nr}] {place}\n{text}\n\n")),
            (nr, false) => out.push_str(&format!("[{nr}] {place} (steht schon oben)\n\n")),
        }
    }
    out.trim_end().to_owned()
}

fn format_folder(path: &str, entries: &[FolderEntry]) -> String {
    let name = if path.trim_matches('/').is_empty() {
        "Archiv".to_owned()
    } else {
        path.trim_matches('/').to_owned()
    };
    if entries.is_empty() {
        return format!("Ordner leer oder unbekannt: {name}");
    }
    let mut out = format!("Inhalt von {name}:\n");
    for e in entries {
        if e.is_file {
            out.push_str(&format!("{}\n", e.name));
        } else {
            out.push_str(&format!("{}/ ({} Dokumente)\n", e.name, e.docs));
        }
    }
    if entries.len() as i64 >= FOLDER_LIMIT {
        out.push_str("(Liste gekürzt)\n");
    }
    out.trim_end().to_owned()
}

/// Werkzeuge im OpenAI-Format.
fn tools() -> Value {
    json!([
        {"type": "function", "function": {
            "name": "suchen",
            "description": "Durchsucht das Firmenarchiv nach Bedeutung und Wortlaut. Liefert nummerierte Auszüge mit Pfad und Seite.",
            "parameters": {"type": "object", "properties": {
                "anfrage": {"type": "string", "description": "Eigenständige Suchanfrage mit den wichtigen Begriffen: Leistung, Material, Kunde, Ort, Jahr."}
            }, "required": ["anfrage"]}
        }},
        {"type": "function", "function": {
            "name": "seite_lesen",
            "description": "Liest den ganzen Text einer Seite, z. B. wenn ein Auszug abgeschnitten ist.",
            "parameters": {"type": "object", "properties": {
                "pfad": {"type": "string", "description": "Pfad wie in den Quellen oder im Ordner, z. B. Angebote/2019/Kita.pdf"},
                "seite": {"type": "integer", "description": "Seitennummer (ab 1); weglassen für den Anfang"}
            }, "required": ["pfad"]}
        }},
        {"type": "function", "function": {
            "name": "ordner_auflisten",
            "description": "Zeigt Unterordner und Dateien eines Ordners im Archiv, z. B. alle Projekte eines Jahres oder Kunden.",
            "parameters": {"type": "object", "properties": {
                "pfad": {"type": "string", "description": "Ordnerpfad; leer für die oberste Ebene"}
            }, "required": ["pfad"]}
        }}
    ])
}

pub const AGENT_PROMPT: &str = "Du bist der Dokumenten-Assistent eines GaLaBau-Betriebs und hast Zugriff auf das Firmenarchiv: Angebote, Leistungsverzeichnisse, Rechnungen, Schriftverkehr, Bautagebücher, Lieferantenbelege. Viele Dokumente sind eingescannt und per Texterkennung gelesen, also nicht fehlerfrei. Der Pfad nennt meist Jahr, Auftraggeber und Projekt.

So arbeitest du:
- Suche zuerst mit `suchen`. Formuliere eine eigenständige Suchanfrage mit den wichtigen Begriffen (Leistung, Material, Kunde, Ort, Jahr). Die Suche findet Dokumente auch über ihren Pfad, deshalb gehören Kunde und Ort in die Anfrage. Löse Bezüge auf frühere Fragen auf, z. B. wird aus „und 2020?“ die vollständige Anfrage.
- Prüfe, ob die Quelle zum gefragten Kunden oder Projekt passt. Ein ähnlicher Text aus einem anderen Projekt beantwortet die Frage nicht.
- Passt nichts, suche noch einmal mit anderen Wörtern oder Fachbegriffen (z. B. Pflaster, Betonsteinpflaster, Verbundpflaster) oder sieh mit `ordner_auflisten` im passenden Jahresordner nach. Antworte erst nach mindestens zwei Suchen, dass du nichts gefunden hast.
- Ist ein Auszug abgeschnitten, lies die Seite mit `seite_lesen`.
- Nach höchstens drei Runden mit Werkzeugen antwortest du.
- Belege von Lieferanten (z. B. im Ordner Lieferantendokumentation, Angebote und Auftragsbestätigungen von Baustoffhändlern oder Baumschulen) zeigen Einkaufspreise. Eigene Angebote, Leistungsverzeichnisse und Rechnungen an Kunden zeigen Verkaufspreise. Sage in der Antwort, was was ist.

Zahlen in den Quellen stehen im deutschen Format: Komma als Dezimaltrennzeichen, Punkt für Tausender (30,000 = 30; 1.075,00 = 1075).

Antwort: Schreibe schlichten Text ohne Markdown, also keine Tabellen, kein Fettdruck und keine Überschriften mit #. Aufzählungen mit „-“ sind erlaubt. Beantworte die Frage konkret aus den Quellen. Nenne Preise, Mengen, Daten, Firmen und Projekte genau so, wie sie dort stehen, und setze hinter jede Aussage die Quellennummer als [1], [2] usw. Passen mehrere Projekte oder Jahre, liste sie einzeln auf. Beantworten die Quellen die Frage nur teilweise, sage, was du gefunden hast und was fehlt. Nur wenn keine Quelle zur Frage passt, antworte genau mit: \"Dazu habe ich in den Dokumenten nichts gefunden.\" Erfinde nichts und nutze kein Wissen außerhalb der Quellen. Antworte auf Deutsch.";

/// System-Prompt mit Datum, frühere Runden und die Frage (Werkzeug-Weg).
fn agent_messages(question: &str, history: &[ChatTurn]) -> Vec<ChatMessage> {
    let today = chrono::Local::now().format("%d.%m.%Y");
    let mut messages = vec![ChatMessage::system(format!(
        "{AGENT_PROMPT}\nHeute ist der {today}."
    ))];
    push_history(&mut messages, history);
    messages.push(ChatMessage::user(question));
    messages
}

fn push_history(messages: &mut Vec<ChatMessage>, history: &[ChatTurn]) {
    let skip = history.len().saturating_sub(MAX_HISTORY_TURNS);
    for turn in &history[skip..] {
        messages.push(ChatMessage::user(turn.question.clone()));
        messages.push(ChatMessage::assistant(turn.answer.clone()));
    }
}

pub const SYSTEM_PROMPT: &str = "Du bist der Dokumenten-Assistent eines GaLaBau-Betriebs. Du bekommst nummerierte Auszüge aus den Firmendokumenten (Angebote, Leistungsverzeichnisse, Rechnungen, Schriftverkehr, Bautagebücher). Viele sind eingescannt und per Texterkennung gelesen, also nicht fehlerfrei. Der Pfad vor jedem Auszug nennt meist Jahr, Auftraggeber und Projekt. Beantworte die Frage konkret aus diesen Auszügen: Nenne Preise, Mengen, Daten, Firmen und Projekte genau so, wie sie dort stehen, und setze hinter jede Aussage die Quelle als [1], [2] usw. Passen mehrere Projekte oder Jahre, liste sie einzeln auf. Beantworten die Auszüge die Frage nur teilweise, sage, was du gefunden hast und was fehlt. Nur wenn kein Auszug zur Frage passt, antworte genau mit: \"Dazu habe ich in den Dokumenten nichts gefunden.\" Erfinde nichts und nutze kein Wissen außerhalb der Auszüge. Zahlen stehen im deutschen Format (Komma als Dezimaltrennzeichen). Schreibe schlichten Text ohne Markdown (keine Tabellen, kein Fettdruck), Aufzählungen mit „-“ sind erlaubt. Antworte auf Deutsch.";

/// llama-server lehnt Anfragen über der Kontextgröße mit 400 ab.
fn is_context_overflow(e: &ChatError) -> bool {
    matches!(e, ChatError::Llm(llm::LlmError::Status { status: 400, body })
        if body.contains("context size"))
}

/// Einfacher Weg: System-Prompt, frühere Runden und die Frage samt nummerierten Quellen.
pub fn build_messages(question: &str, history: &[ChatTurn], hits: &[Hit]) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage::system(SYSTEM_PROMPT)];
    push_history(&mut messages, history);

    let mut user = String::from("Quellen:\n");
    for (i, hit) in hits.iter().enumerate() {
        user.push_str(&format!(
            "\n[{}] {}\n{}\n",
            i + 1,
            place(hit),
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

fn to_source(nr: usize, hit: &Hit) -> ChatSource {
    ChatSource {
        nr: u32::try_from(nr).unwrap_or(u32::MAX),
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
    use llm::{FunctionCall, Role};
    use retrieval::HashEmbedder;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use uuid::Uuid;

    struct FakeSearch {
        hits: Vec<Hit>,
    }
    impl Search for FakeSearch {
        async fn search(
            &self,
            _q: &str,
            emb: Vec<f32>,
            limit: usize,
        ) -> Result<Vec<Hit>, ChatError> {
            assert_eq!(emb.len(), 32, "Anfrage wurde nicht eingebettet");
            Ok(self.hits.iter().take(limit).cloned().collect())
        }
        async fn page(&self, rel_path: &str, page: Option<u32>) -> Result<Option<Hit>, ChatError> {
            Ok((rel_path == "a.pdf").then(|| hit(rel_path, page, "Ganze Seite mit Kies 12 EUR")))
        }
        async fn folder(&self, path: &str) -> Result<Vec<FolderEntry>, ChatError> {
            Ok(if path.is_empty() {
                vec![
                    FolderEntry {
                        name: "Angebote".into(),
                        is_file: false,
                        docs: 12,
                    },
                    FolderEntry {
                        name: "liste.xlsx".into(),
                        is_file: true,
                        docs: 1,
                    },
                ]
            } else {
                Vec::new()
            })
        }
    }

    struct NoRerank;
    impl Rerank for NoRerank {
        async fn rerank(&self, _q: &str, _d: Vec<String>) -> Option<Vec<f32>> {
            None
        }
    }

    /// Kehrt die Reihenfolge um (spätere Kandidaten bekommen höhere Scores).
    struct ReverseRerank;
    impl Rerank for ReverseRerank {
        async fn rerank(&self, _q: &str, d: Vec<String>) -> Option<Vec<f32>> {
            Some((0..d.len()).map(|i| i as f32).collect())
        }
    }

    type Step = Result<Turn, fn() -> ChatError>;

    #[derive(Default)]
    struct FakeLlm {
        script: Mutex<VecDeque<Step>>,
        /// Je Aufruf: Nachrichten und `tool_choice` (`None` = ohne Werkzeuge).
        calls: Mutex<Vec<(Vec<ChatMessage>, Option<String>)>>,
        /// Wie llama-server: längere Prompts (in Zeichen) werden mit 400 abgelehnt.
        max_chars: usize,
    }
    impl Llm for FakeLlm {
        async fn turn(
            &self,
            messages: Vec<ChatMessage>,
            tools: Option<(&Value, &str)>,
            on_delta: &mut (dyn FnMut(&str) -> bool + Send),
        ) -> Result<Turn, ChatError> {
            let len: usize = messages.iter().map(|m| m.content.chars().count()).sum();
            self.calls
                .lock()
                .unwrap()
                .push((messages, tools.map(|(_, c)| c.to_owned())));
            if len > self.max_chars {
                return Err(overflow());
            }
            let step = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("Skript zu kurz");
            let mut turn = step.map_err(|f| f())?;
            let mut sent = String::new();
            for w in turn.content.split_inclusive(' ') {
                sent.push_str(w);
                if !on_delta(w) {
                    turn.content = sent;
                    break;
                }
            }
            Ok(turn)
        }
    }

    fn overflow() -> ChatError {
        ChatError::Llm(llm::LlmError::Status {
            status: 400,
            body: "request exceeds the available context size".into(),
        })
    }

    fn hit(path: &str, page: Option<u32>, text: &str) -> Hit {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Hit {
            chunk_id: Uuid::from_u128(u128::from(NEXT.fetch_add(1, Ordering::Relaxed))),
            rel_path: path.into(),
            page,
            text: text.into(),
            score: 0.03,
        }
    }

    fn text(s: &str) -> Step {
        Ok(Turn {
            content: s.into(),
            tool_calls: Vec::new(),
        })
    }

    fn tools_step(calls: &[(&str, &str)]) -> Step {
        Ok(Turn {
            content: String::new(),
            tool_calls: calls
                .iter()
                .enumerate()
                .map(|(i, (name, args))| ToolCall {
                    id: format!("c{i}"),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: (*name).into(),
                        arguments: (*args).into(),
                    },
                })
                .collect(),
        })
    }

    fn service<R: Rerank>(
        hits: Vec<Hit>,
        reranker: R,
        script: Vec<Step>,
    ) -> ChatService<HashEmbedder, FakeSearch, R, FakeLlm> {
        ChatService::new(
            HashEmbedder::new(32),
            FakeSearch { hits },
            reranker,
            FakeLlm {
                script: Mutex::new(script.into()),
                max_chars: usize::MAX,
                ..Default::default()
            },
        )
    }

    /// Antwort plus alle Ereignisse.
    async fn ask<R: Rerank>(
        svc: &ChatService<HashEmbedder, FakeSearch, R, FakeLlm>,
        q: &str,
        history: &[ChatTurn],
    ) -> (Result<ChatResponse, ChatError>, Vec<ChatStreamEvent>) {
        let events = Mutex::new(Vec::new());
        let sink = |e| events.lock().unwrap().push(e);
        let r = svc.answer(q, history, &sink).await;
        (r, events.into_inner().unwrap())
    }

    fn streamed(events: &[ChatStreamEvent]) -> String {
        let mut s = String::new();
        for e in events {
            match e {
                ChatStreamEvent::Delta { text } => s.push_str(text),
                ChatStreamEvent::Reset => s.clear(),
                _ => {}
            }
        }
        s
    }

    #[tokio::test]
    async fn agent_searches_then_answers_with_cited_sources() {
        let long = "Pflaster ".repeat(400);
        let svc = service(
            vec![
                hit("Angebote/2019/Kita.pdf", Some(3), &long),
                hit("Preise.xlsx", None, "Kies 12 EUR"),
                hit("c.pdf", Some(1), "anderes"),
            ],
            NoRerank,
            vec![
                tools_step(&[("suchen", r#"{"anfrage":"Kies Preis"}"#)]),
                text("Kies kostet 12 EUR [2]."),
            ],
        );
        let history = [ChatTurn {
            question: "Hallo?".into(),
            answer: "Hallo.".into(),
        }];
        let (resp, events) = ask(&svc, "Was kostet Kies?", &history).await;
        let resp = resp.unwrap();

        assert_eq!(resp.answer, "Kies kostet 12 EUR [2].");
        assert_eq!(resp.sources.len(), 1, "nur die zitierte Quelle");
        assert_eq!(resp.sources[0].nr, 2);
        assert_eq!(resp.sources[0].path, "Preise.xlsx");
        assert!(events.contains(&ChatStreamEvent::Status {
            text: "Suche: Kies Preis".into()
        }));
        assert_eq!(streamed(&events), resp.answer);

        let calls = svc.llm.calls.lock().unwrap();
        assert_eq!(calls[0].1.as_deref(), Some("required"));
        assert_eq!(calls[1].1.as_deref(), Some("auto"));
        let first = &calls[0].0;
        assert_eq!(first[0].role, Role::System);
        assert!(first[0].content.contains("Heute ist der"));
        assert_eq!(first[1].content, "Hallo?");
        assert_eq!(first[2].content, "Hallo.");
        assert_eq!(first[3].content, "Was kostet Kies?");
        let tool = calls[1].0.last().unwrap();
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("c0"));
        assert!(
            tool.content
                .contains("[1] Angebote/2019/Kita.pdf, Seite 3\n")
        );
        assert!(tool.content.contains("[2] Preise.xlsx\nKies 12 EUR"));
        assert!(tool.content.chars().count() < 1500 + 400, "Treffer gekürzt");
    }

    #[tokio::test]
    async fn same_chunk_keeps_its_number_across_searches() {
        let svc = service(
            vec![hit("a.pdf", Some(1), "Kies 12 EUR")],
            NoRerank,
            vec![
                tools_step(&[("suchen", r#"{"anfrage":"Kies"}"#)]),
                tools_step(&[("suchen", r#"{"anfrage":"Schotter"}"#)]),
                text("12 EUR [1]"),
            ],
        );
        let (resp, _) = ask(&svc, "Kies?", &[]).await;
        assert_eq!(resp.unwrap().sources.len(), 1);
        let calls = svc.llm.calls.lock().unwrap();
        let second = &calls[2].0.last().unwrap().content;
        assert_eq!(second, "[1] a.pdf, Seite 1 (steht schon oben)");
    }

    #[tokio::test]
    async fn page_and_folder_tools() {
        let svc = service(
            vec![],
            NoRerank,
            vec![
                tools_step(&[
                    ("seite_lesen", r#"{"pfad":"a.pdf","seite":"2"}"#),
                    ("ordner_auflisten", r#"{"pfad":""}"#),
                    ("seite_lesen", r#"{"pfad":"fehlt.pdf"}"#),
                    ("suchen", r#"{"anfrage":"x"}"#),
                ]),
                text("Steht auf Seite 2 [1]."),
            ],
        );
        let (resp, events) = ask(&svc, "Was steht in a.pdf?", &[]).await;
        let resp = resp.unwrap();
        assert_eq!(resp.sources[0].page, Some(2));
        assert!(events.contains(&ChatStreamEvent::Status {
            text: "Lese a.pdf, Seite 2".into()
        }));
        let calls = svc.llm.calls.lock().unwrap();
        let msgs = &calls[1].0;
        let n = msgs.len();
        assert_eq!(
            msgs[n - 4].content,
            "[1] a.pdf, Seite 2\nGanze Seite mit Kies 12 EUR"
        );
        assert_eq!(
            msgs[n - 3].content,
            "Inhalt von Archiv:\nAngebote/ (12 Dokumente)\nliste.xlsx"
        );
        assert!(msgs[n - 2].content.starts_with("Nicht gefunden"));
        assert!(msgs[n - 1].content.starts_with("Übersprungen"));
    }

    #[tokio::test]
    async fn last_round_allows_no_tools() {
        let search = r#"{"anfrage":"Kies"}"#;
        let svc = service(
            vec![hit("a.pdf", None, "Kies")],
            NoRerank,
            vec![
                tools_step(&[("suchen", search)]),
                tools_step(&[("suchen", search)]),
                tools_step(&[("suchen", search)]),
                text("Kies [1]"),
            ],
        );
        let (resp, _) = ask(&svc, "Kies?", &[]).await;
        assert_eq!(resp.unwrap().answer, "Kies [1]");
        let calls = svc.llm.calls.lock().unwrap();
        assert_eq!(calls.len(), MAX_ROUNDS);
        assert_eq!(calls[MAX_ROUNDS - 1].1.as_deref(), Some("none"));
    }

    #[tokio::test]
    async fn answer_without_search_is_discarded_for_classic_path() {
        let svc = service(
            vec![hit("a.pdf", Some(1), "Kies 12 EUR")],
            NoRerank,
            vec![text("geraten"), text("Kies kostet 12 EUR [1]")],
        );
        let (resp, events) = ask(&svc, "Kies?", &[]).await;
        let resp = resp.unwrap();
        assert_eq!(resp.answer, "Kies kostet 12 EUR [1]");
        assert_eq!(resp.sources[0].nr, 1);
        assert_eq!(streamed(&events), resp.answer, "„geraten“ wurde verworfen");
        let calls = svc.llm.calls.lock().unwrap();
        assert_eq!(calls[1].1, None, "einfacher Weg ohne Werkzeuge");
        assert!(
            calls[1]
                .0
                .last()
                .unwrap()
                .content
                .contains("[1] a.pdf, Seite 1")
        );
    }

    #[tokio::test]
    async fn overflow_after_search_answers_from_found_sources() {
        let svc = service(
            vec![hit("a.pdf", None, "Kies 12 EUR")],
            NoRerank,
            vec![
                tools_step(&[("suchen", r#"{"anfrage":"Kies"}"#)]),
                Err(overflow),
                text("12 EUR [1]"),
            ],
        );
        let (resp, _) = ask(&svc, "Kies?", &[]).await;
        assert_eq!(resp.unwrap().sources.len(), 1);
        let calls = svc.llm.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[2].1, None);
    }

    #[tokio::test]
    async fn not_found_answer_drops_sources() {
        let svc = service(
            vec![],
            NoRerank,
            vec![
                tools_step(&[("suchen", r#"{"anfrage":"x"}"#)]),
                text(NOT_FOUND),
            ],
        );
        let (resp, _) = ask(&svc, "Frage", &[]).await;
        let resp = resp.unwrap();
        assert_eq!(resp.answer, NOT_FOUND);
        assert!(resp.sources.is_empty());
        let calls = svc.llm.calls.lock().unwrap();
        assert!(
            calls[1]
                .0
                .last()
                .unwrap()
                .content
                .starts_with("Keine Treffer")
        );
    }

    #[tokio::test]
    async fn classic_without_hits_needs_no_llm_answer() {
        let svc = service(vec![], NoRerank, vec![text("ohne Suche")]);
        let (resp, _) = ask(&svc, "Irgendwas?", &[]).await;
        assert_eq!(resp.unwrap().answer, NOT_FOUND);
        assert_eq!(svc.llm.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reranker_reorders_hits() {
        let svc = service(
            vec![
                hit("a.pdf", None, "a"),
                hit("b.pdf", None, "b"),
                hit("c.pdf", None, "c"),
            ],
            ReverseRerank,
            vec![],
        );
        let hits = svc.retrieve("x", 2).await.unwrap();
        let paths: Vec<_> = hits.iter().map(|h| h.rel_path.as_str()).collect();
        assert_eq!(paths, ["c.pdf", "b.pdf"]);
        assert_eq!(hits[0].score, 2.0);
    }

    #[tokio::test]
    async fn too_long_prompt_retries_with_fewer_sources_then_without_history() {
        let hits: Vec<Hit> = (0..8)
            .map(|i| hit(&format!("d{i}.pdf"), None, &"x".repeat(1000)))
            .collect();
        let mut svc = service(hits, NoRerank, vec![text("Antwort [1]")]);
        let history = vec![ChatTurn {
            question: "alt".into(),
            answer: "y".repeat(3000),
        }];

        // Schon die erste Runde ist zu lang (Verlauf), dann der einfache Weg:
        // 8 -> 4 -> 2 -> 1 Quellen, zuletzt ohne Verlauf.
        svc.llm.max_chars = SYSTEM_PROMPT.chars().count() + 1300;
        let (resp, _) = ask(&svc, "Frage", &history).await;
        let resp = resp.unwrap();
        assert_eq!(resp.answer, "Antwort [1]");
        assert_eq!(resp.sources.len(), 1);
        let last_len = svc.llm.calls.lock().unwrap().last().unwrap().0.len();
        assert_eq!(last_len, 2, "System + Frage, kein Verlauf");

        // Passt gar nichts, kommt der Fehler durch.
        svc.llm.max_chars = 10;
        assert!(ask(&svc, "Frage", &history).await.0.is_err());
    }

    #[tokio::test]
    async fn unreachable_reranker_is_skipped_for_a_while() {
        let client = LlmClient::new("http://127.0.0.1:1", Duration::from_secs(2)).unwrap();
        let r = LlmReranker::new(Some(client));
        assert_eq!(r.rerank("q", vec!["a".into()]).await, None);
        let until = r.down_until.lock().unwrap().expect("Pause gesetzt");
        assert!(until > Instant::now() + Duration::from_secs(200));
        assert_eq!(LlmReranker::new(None).rerank("q", vec![]).await, None);
    }

    #[tokio::test]
    async fn looping_answer_is_cut_and_deduplicated() {
        let line = "- Godelmann Tetrago grau 60/30/6 cm, 23,97 € je m² [1]\n";
        let looping = format!("Gefunden:\n{}", line.repeat(40));
        let svc = service(
            vec![hit("a.pdf", None, "Tetrago")],
            NoRerank,
            vec![
                tools_step(&[("suchen", r#"{"anfrage":"Godelmann"}"#)]),
                text(&looping),
            ],
        );
        let (resp, events) = ask(&svc, "Godelmann?", &[]).await;
        let resp = resp.unwrap();
        assert_eq!(resp.answer, format!("Gefunden:\n{}", line.trim_end()));
        assert!(
            streamed(&events).matches("Godelmann").count() <= 4,
            "Abbruch nach der dritten Wiederholung"
        );
    }

    #[test]
    fn repeats_needs_three_complete_long_lines() {
        let l = "Diese Zeile ist lang genug für den Test\n";
        assert!(!repeats(&l.repeat(2)));
        assert!(repeats(&l.repeat(3)));
        assert!(
            !repeats(&format!("{}{}", l.repeat(2), l.trim_end())),
            "letzte Zeile unfertig"
        );
        assert!(!repeats(&"- kurz\n".repeat(10)));
    }

    #[test]
    fn cited_numbers_parses_brackets() {
        let got = cited_numbers("a [1], b [2, 5] c [3-4] [x] [12] [] [1a] [7");
        assert_eq!(got.into_iter().collect::<Vec<_>>(), [1, 2, 3, 4, 5, 12]);
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
}
