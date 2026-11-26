//! Client für `llama-server` (OpenAI-kompatible API, `POST /v1/chat/completions`).
//!
//! Der Client kennt nur, was GaLa-AI braucht: Chat-Completion (auch gestreamt und mit
//! Werkzeugaufrufen), per JSON-Schema eingeschränkte Ausgabe (llama-server setzt daraus eine
//! Grammatik), Reranking (`POST /v1/rerank`, eigener llama-server mit `--reranking`) und den
//! Health-Check. Anfrage-Bau und Antwort-Parsing sind reine Funktionen und ohne Netzwerk testbar.

use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP-Fehler beim Zugriff auf llama-server")]
    Http(#[from] reqwest::Error),
    #[error("llama-server antwortete mit Status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Antwort von llama-server ist ungültig: {0}")]
    InvalidResponse(String),
    #[error("Antwort passt nicht zum erwarteten Typ")]
    Deserialize(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Werkzeugaufruf des Modells (OpenAI-Format); `arguments` ist JSON als Text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Nur bei `assistant`: die Werkzeugaufrufe dieser Runde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Nur bei `tool`: auf welchen Aufruf das Ergebnis antwortet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    /// Antwort des Modells, die Werkzeuge aufruft.
    pub fn assistant_tools(content: impl Into<String>, calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls: Some(calls),
            ..Self::new(Role::Assistant, content)
        }
    }
    /// Ergebnis eines Werkzeugaufrufs.
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call_id.into()),
            ..Self::new(Role::Tool, content)
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatOptions {
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    /// DRY-Sampler von llama-server (bremst wiederholte Textstücke, z. B. endlose
    /// Tabellenzeilen). `None`: aus.
    pub dry_multiplier: Option<f32>,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            max_tokens: Some(1024),
            dry_multiplier: None,
        }
    }
}

/// Baut den Request-Body. `schema` ist `(Name, JSON-Schema)` für `response_format`.
pub fn build_chat_body(
    model: Option<&str>,
    messages: &[ChatMessage],
    options: &ChatOptions,
    schema: Option<(&str, &Value)>,
) -> Value {
    let mut body = json!({
        "messages": messages,
        "temperature": options.temperature,
        "stream": false,
    });
    let obj = body.as_object_mut().expect("Objekt");
    if let Some(model) = model {
        obj.insert("model".into(), json!(model));
    }
    if let Some(max) = options.max_tokens {
        obj.insert("max_tokens".into(), json!(max));
    }
    if let Some(dry) = options.dry_multiplier {
        obj.insert("dry_multiplier".into(), json!(dry));
    }
    if let Some((name, schema)) = schema {
        obj.insert(
            "response_format".into(),
            json!({
                "type": "json_schema",
                "json_schema": { "name": name, "strict": true, "schema": schema },
            }),
        );
    }
    body
}

/// Holt den Text der ersten Antwortauswahl.
pub fn parse_chat_response(body: &str) -> Result<String, LlmError> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| LlmError::InvalidResponse("choices[0].message.content fehlt".into()))
}

/// Parst die (schema-konforme) Antwort in einen Typ. Toleriert ```json-Zäune.
pub fn parse_json_content<T: DeserializeOwned>(content: &str) -> Result<T, LlmError> {
    let trimmed = content.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.strip_suffix("```").unwrap_or(s))
        .unwrap_or(trimmed)
        .trim();
    serde_json::from_str(trimmed).map_err(LlmError::Deserialize)
}

/// Body für eine gestreamte Anfrage, optional mit Werkzeugen (`tools`, `tool_choice`:
/// `auto`, `required` oder `none`).
pub fn build_stream_body(
    model: Option<&str>,
    messages: &[ChatMessage],
    options: &ChatOptions,
    tools: Option<(&Value, &str)>,
) -> Value {
    let mut body = build_chat_body(model, messages, options, None);
    let obj = body.as_object_mut().expect("Objekt");
    obj.insert("stream".into(), json!(true));
    if let Some((tools, choice)) = tools {
        obj.insert("tools".into(), tools.clone());
        obj.insert("tool_choice".into(), json!(choice));
    }
    body
}

/// Ergebnis einer (gestreamten) Runde: Text und/oder Werkzeugaufrufe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

/// Liest den Server-Sent-Events-Strom von llama-server (`data: {...}` je Zeile, Ende mit
/// `data: [DONE]`). Bytes kommen in beliebigen Stücken, auch mitten in UTF-8-Zeichen.
#[derive(Debug, Default)]
pub struct StreamParser {
    buf: Vec<u8>,
    turn: Turn,
}

impl StreamParser {
    /// Verarbeitet ein Stück und liefert die neuen Text-Deltas.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, LlmError> {
        self.buf.extend_from_slice(bytes);
        let mut deltas = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(d) = self.line(line.trim())? {
                deltas.push(d);
            }
        }
        Ok(deltas)
    }

    fn line(&mut self, line: &str) -> Result<Option<String>, LlmError> {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(None);
        };
        if data.is_empty() || data == "[DONE]" {
            return Ok(None);
        }
        let v: Value =
            serde_json::from_str(data).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        if let Some(err) = v.get("error") {
            let status = err.get("code").and_then(Value::as_u64).unwrap_or(500);
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(LlmError::Status {
                status: u16::try_from(status).unwrap_or(500),
                body: msg.chars().take(500).collect(),
            });
        }
        let Some(delta) = v.pointer("/choices/0/delta") else {
            return Ok(None);
        };
        for tc in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let i = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            while self.turn.tool_calls.len() <= i {
                self.turn.tool_calls.push(ToolCall {
                    id: String::new(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                });
            }
            let call = &mut self.turn.tool_calls[i];
            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                call.id.push_str(id);
            }
            if let Some(n) = tc.pointer("/function/name").and_then(Value::as_str) {
                call.function.name.push_str(n);
            }
            if let Some(a) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                call.function.arguments.push_str(a);
            }
        }
        match delta.get("content").and_then(Value::as_str) {
            Some(c) if !c.is_empty() => {
                self.turn.content.push_str(c);
                Ok(Some(c.to_owned()))
            }
            _ => Ok(None),
        }
    }

    /// Restlicher Puffer (letzte Zeile ohne Zeilenumbruch) und Ergebnis.
    pub fn finish(mut self) -> Result<Turn, LlmError> {
        let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
        self.line(rest.trim())?;
        for (i, c) in self.turn.tool_calls.iter_mut().enumerate() {
            if c.id.is_empty() {
                c.id = format!("call_{i}");
            }
        }
        Ok(self.turn)
    }
}

/// Liest `{"results":[{"index":i,"relevance_score":s}]}` und gibt die Scores in
/// Eingabe-Reihenfolge zurück.
pub fn parse_rerank_response(body: &str, n: usize) -> Result<Vec<f32>, LlmError> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let results = v
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::InvalidResponse("results fehlt".into()))?;
    let mut scores = vec![f32::NEG_INFINITY; n];
    for r in results {
        let i = r.get("index").and_then(Value::as_u64).map(|i| i as usize);
        let s = r.get("relevance_score").and_then(Value::as_f64);
        match (i, s) {
            (Some(i), Some(s)) if i < n => scores[i] = s as f32,
            _ => {
                return Err(LlmError::InvalidResponse(format!(
                    "ungültiges Ergebnis: {r}"
                )));
            }
        }
    }
    Ok(scores)
}

/// Ein Slot von llama-server (`GET /slots`); je Slot läuft eine Anfrage gleichzeitig.
#[derive(Debug, Clone, Deserialize)]
pub struct SlotInfo {
    pub id: u32,
    pub n_ctx: u32,
    pub is_processing: bool,
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: Option<String>,
}

impl LlmClient {
    /// `base_url` z. B. `http://gala-llm:8080`. `timeout` gilt je Anfrage (Generieren kann dauern).
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(timeout)
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            model: None,
        })
    }

    /// Modellname für die Anfrage; llama-server ignoriert ihn bei nur einem geladenen Modell.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// `GET /health`: `true`, wenn der Server bereit ist (Status 200).
    pub async fn health(&self) -> bool {
        match self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// `GET /slots`: Belegung und Kontextgröße je Slot (für das Admin-Panel).
    pub async fn slots(&self) -> Result<Vec<SlotInfo>, LlmError> {
        let resp = self
            .http
            .get(format!("{}/slots", self.base_url))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(resp.json().await?)
    }

    async fn post_chat(&self, body: &Value) -> Result<String, LlmError> {
        let resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        parse_chat_response(&text)
    }

    /// Freie Chat-Antwort.
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        options: &ChatOptions,
    ) -> Result<String, LlmError> {
        let body = build_chat_body(self.model.as_deref(), messages, options, None);
        self.post_chat(&body).await
    }

    /// Gestreamte Runde, optional mit Werkzeugen. Jedes Text-Stück geht sofort an `on_delta`;
    /// gibt es `false` zurück, bricht die Anfrage ab (llama-server hört dann auf zu rechnen) und
    /// das Bisherige kommt zurück.
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        options: &ChatOptions,
        tools: Option<(&Value, &str)>,
        on_delta: &mut (dyn FnMut(&str) -> bool + Send),
    ) -> Result<Turn, LlmError> {
        let body = build_stream_body(self.model.as_deref(), messages, options, tools);
        let mut resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        let mut parser = StreamParser::default();
        while let Some(chunk) = resp.chunk().await? {
            for d in parser.feed(&chunk)? {
                if !on_delta(&d) {
                    return parser.finish();
                }
            }
        }
        parser.finish()
    }

    /// `POST /v1/rerank`: Relevanz jedes Dokuments zur Anfrage, in Eingabe-Reihenfolge.
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, LlmError> {
        let body = json!({ "query": query, "documents": documents, "top_n": documents.len() });
        let resp = self
            .http
            .post(format!("{}/v1/rerank", self.base_url))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        parse_rerank_response(&text, documents.len())
    }

    /// Antwort, die per JSON-Schema eingeschränkt ist und als `T` zurückkommt.
    pub async fn chat_json<T: DeserializeOwned>(
        &self,
        messages: &[ChatMessage],
        options: &ChatOptions,
        schema_name: &str,
        schema: &Value,
    ) -> Result<T, LlmError> {
        let body = build_chat_body(
            self.model.as_deref(),
            messages,
            options,
            Some((schema_name, schema)),
        );
        let content = self.post_chat(&body).await?;
        parse_json_content(&content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_without_schema() {
        let msgs = [
            ChatMessage::system("Sei knapp."),
            ChatMessage::user("Hallo"),
        ];
        let body = build_chat_body(
            None,
            &msgs,
            &ChatOptions {
                temperature: 0.5,
                max_tokens: Some(64),
                dry_multiplier: Some(0.8),
            },
            None,
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "Hallo");
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_tokens"], 64);
        assert!((body["dry_multiplier"].as_f64().unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(body["stream"], false);
        assert!(body.get("response_format").is_none());
        assert!(body.get("model").is_none());
    }

    #[test]
    fn body_with_schema_and_model() {
        let schema =
            json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]});
        let body = build_chat_body(
            Some("qwen"),
            &[ChatMessage::user("x")],
            &ChatOptions {
                temperature: 0.0,
                max_tokens: None,
                dry_multiplier: None,
            },
            Some(("antwort", &schema)),
        );
        assert_eq!(body["model"], "qwen");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("dry_multiplier").is_none());
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "antwort");
        assert_eq!(body["response_format"]["json_schema"]["schema"], schema);
    }

    #[test]
    fn parses_response() {
        let raw = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"Hallo Welt"},"finish_reason":"stop"}],"usage":{}}"#;
        assert_eq!(parse_chat_response(raw).unwrap(), "Hallo Welt");
    }

    #[test]
    fn rejects_response_without_content() {
        assert!(matches!(
            parse_chat_response(r#"{"choices":[]}"#),
            Err(LlmError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_chat_response("kein json"),
            Err(LlmError::InvalidResponse(_))
        ));
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Antwort {
        ok: bool,
        n: u32,
    }

    #[test]
    fn parses_typed_json_content() {
        let v: Antwort = parse_json_content(r#"{"ok":true,"n":3}"#).unwrap();
        assert_eq!(v, Antwort { ok: true, n: 3 });
        let v: Antwort = parse_json_content("```json\n{\"ok\":false,\"n\":1}\n```").unwrap();
        assert_eq!(v, Antwort { ok: false, n: 1 });
        assert!(matches!(
            parse_json_content::<Antwort>(r#"{"ok":"ja"}"#),
            Err(LlmError::Deserialize(_))
        ));
    }

    #[test]
    fn stream_body_with_tools() {
        let tools = json!([{"type": "function", "function": {"name": "suchen"}}]);
        let body = build_stream_body(
            None,
            &[ChatMessage::user("x")],
            &ChatOptions::default(),
            Some((&tools, "required")),
        );
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"], tools);
        assert_eq!(body["tool_choice"], "required");
        let plain = build_stream_body(
            None,
            &[ChatMessage::user("x")],
            &ChatOptions::default(),
            None,
        );
        assert!(plain.get("tools").is_none());
    }

    #[test]
    fn tool_messages_serialize_like_openai() {
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "suchen".into(),
                arguments: r#"{"anfrage":"Kies"}"#.into(),
            },
        };
        let a = serde_json::to_value(ChatMessage::assistant_tools("", vec![call])).unwrap();
        assert_eq!(a["tool_calls"][0]["type"], "function");
        assert_eq!(a["tool_calls"][0]["function"]["name"], "suchen");
        assert!(a.get("tool_call_id").is_none());
        let t = serde_json::to_value(ChatMessage::tool("c1", "[1] a.pdf")).unwrap();
        assert_eq!(t["role"], "tool");
        assert_eq!(t["tool_call_id"], "c1");
        let u = serde_json::to_value(ChatMessage::user("x")).unwrap();
        assert!(u.get("tool_calls").is_none() && u.get("tool_call_id").is_none());
    }

    #[test]
    fn stream_parser_collects_text_split_mid_utf8() {
        let raw = concat!(
            r#"data: {"choices":[{"delta":{"content":"Grü"}}]}"#,
            "\n\n",
            r#"data: {"choices":[{"delta":{"content":"ße [1]"}}]}"#,
            "\n\ndata: [DONE]\n\n"
        );
        let bytes = raw.as_bytes();
        // mitten im „ü“ (2 Byte) teilen
        let cut = raw.find('ü').unwrap() + 1;
        let mut p = StreamParser::default();
        let mut deltas = p.feed(&bytes[..cut]).unwrap();
        deltas.extend(p.feed(&bytes[cut..]).unwrap());
        assert_eq!(deltas, ["Grü", "ße [1]"]);
        let turn = p.finish().unwrap();
        assert_eq!(turn.content, "Grüße [1]");
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn stream_parser_assembles_tool_calls() {
        let raw = concat!(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"suchen","arguments":"{\"anf"}}]}}]}"#,
            "\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"rage\":\"Kies\"}"}}]}}]}"#,
            "\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"ordner_auflisten","arguments":"{}"}}]}}]}"#,
            "\n",
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        let mut p = StreamParser::default();
        assert!(p.feed(raw.as_bytes()).unwrap().is_empty());
        let turn = p.finish().unwrap();
        assert_eq!(turn.tool_calls.len(), 2);
        assert_eq!(turn.tool_calls[0].id, "a");
        assert_eq!(
            turn.tool_calls[0].function.arguments,
            r#"{"anfrage":"Kies"}"#
        );
        assert_eq!(turn.tool_calls[1].id, "call_1");
        assert_eq!(turn.tool_calls[1].function.name, "ordner_auflisten");
    }

    #[test]
    fn stream_parser_reports_server_error() {
        let mut p = StreamParser::default();
        let e = p
            .feed(
                br#"data: {"error":{"code":400,"message":"exceeds the available context size"}}
"#,
            )
            .unwrap_err();
        assert!(
            matches!(e, LlmError::Status { status: 400, ref body } if body.contains("context size"))
        );
    }

    #[test]
    fn rerank_scores_in_input_order() {
        let body =
            r#"{"results":[{"index":1,"relevance_score":2.5},{"index":0,"relevance_score":-1.0}]}"#;
        assert_eq!(parse_rerank_response(body, 2).unwrap(), [-1.0, 2.5]);
        assert!(
            parse_rerank_response(r#"{"results":[{"index":5,"relevance_score":1}]}"#, 2).is_err()
        );
        assert!(parse_rerank_response("{}", 2).is_err());
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
    }

    #[test]
    fn client_trims_trailing_slash() {
        let c = LlmClient::new("http://gala-llm:8080/", Duration::from_secs(1)).unwrap();
        assert_eq!(c.base_url, "http://gala-llm:8080");
    }
}
