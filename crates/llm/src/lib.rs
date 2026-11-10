//! Client für `llama-server` (OpenAI-kompatible API, `POST /v1/chat/completions`).
//!
//! Der Client kennt nur, was GaLa-AI braucht: Chat-Completion, per JSON-Schema eingeschränkte
//! Ausgabe (llama-server setzt daraus eine Grammatik) und den Health-Check. Anfrage-Bau und
//! Antwort-Parsing sind reine Funktionen und ohne Netzwerk testbar.

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
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
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            max_tokens: Some(1024),
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
            },
            None,
        );
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "Hallo");
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_tokens"], 64);
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
            },
            Some(("antwort", &schema)),
        );
        assert_eq!(body["model"], "qwen");
        assert!(body.get("max_tokens").is_none());
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
