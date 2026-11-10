//! Typisierte Huelle um `window.__TAURI__` (`withGlobalTauri: true`).
//! Die Namen und Signaturen entsprechen den Commands in `src-tauri/src/main.rs`.

use api_types::client::{ApiError, ChatRequest, ChatResponse, ChatTurn, HealthResponse, UserInfo};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], catch)]
    async fn listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>) -> Result<JsValue, JsValue>;
}

pub type Res<T> = Result<T, ApiError>;

/// Spiegel von `AppInfo` im Rust-Teil.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub server_url: Option<String>,
    pub has_token: bool,
}

/// Spiegel von `UpdateInfo` im Rust-Teil.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

fn js_err(v: JsValue) -> ApiError {
    if let Ok(e) = serde_wasm_bindgen::from_value::<ApiError>(v.clone()) {
        return e;
    }
    let message = v.as_string().unwrap_or_else(|| format!("{v:?}"));
    ApiError { code: "internal".into(), message }
}

async fn call<A: Serialize, R: DeserializeOwned>(cmd: &str, args: &A) -> Res<R> {
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    let args = args.serialize(&ser).map_err(|e| ApiError { code: "internal".into(), message: e.to_string() })?;
    let out = invoke(cmd, args).await.map_err(js_err)?;
    serde_wasm_bindgen::from_value(out)
        .map_err(|e| ApiError { code: "internal".into(), message: format!("Antwort nicht lesbar: {e}") })
}

/// Leere Argumente als `{}` (nicht `null`).
#[derive(Serialize)]
struct NoArgs {}

#[derive(Serialize)]
struct UrlArg<'a> {
    url: &'a str,
}

#[derive(Serialize)]
struct LoginArgs<'a> {
    username: &'a str,
    password: &'a str,
}

pub async fn app_info() -> Res<AppInfo> {
    call("app_info", &NoArgs {}).await
}

pub async fn test_connection(url: &str) -> Res<HealthResponse> {
    call("test_connection", &UrlArg { url }).await
}

pub async fn save_server_url(url: &str) -> Res<String> {
    call("save_server_url", &UrlArg { url }).await
}

pub async fn health() -> Res<HealthResponse> {
    call("health", &NoArgs {}).await
}

pub async fn login(username: &str, password: &str) -> Res<UserInfo> {
    call("login", &LoginArgs { username, password }).await
}

pub async fn logout() -> Res<()> {
    call("logout", &NoArgs {}).await
}

pub async fn me() -> Res<UserInfo> {
    call("me", &NoArgs {}).await
}

pub async fn chat(question: &str, history: Vec<ChatTurn>) -> Res<ChatResponse> {
    // `ChatRequest` aus api-types dient nur als Form; der Command nimmt die Felder einzeln.
    let ChatRequest { question, history } = ChatRequest { question: question.to_owned(), history };
    call("chat", &ChatArgs { question: &question, history: &history }).await
}

#[derive(Serialize)]
struct ChatArgs<'a> {
    question: &'a str,
    history: &'a [ChatTurn],
}

pub async fn check_update() -> Res<Option<UpdateInfo>> {
    call("check_update", &NoArgs {}).await
}

pub async fn install_update() -> Res<()> {
    call("install_update", &NoArgs {}).await
}

/// Meldet den Download-Fortschritt (`downloaded`, `total`) waehrend `install_update`.
pub fn on_update_progress(cb: impl Fn(u64, Option<u64>) + 'static) {
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
        let payload = js_sys::Reflect::get(&ev, &JsValue::from_str("payload")).unwrap_or(JsValue::UNDEFINED);
        if let Ok(p) = serde_wasm_bindgen::from_value::<Progress>(payload) {
            cb(p.downloaded, p.total);
        }
    });
    wasm_bindgen_futures::spawn_local(async move {
        let _ = listen("update-progress", &closure).await;
        closure.forget();
    });
}
