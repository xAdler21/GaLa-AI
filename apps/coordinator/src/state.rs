//! Gemeinsamer Anwendungszustand.

use std::sync::Arc;
use std::time::Duration;

use db::PgPool;

use crate::auth::LoginLimiter;
use crate::chatbot::{ChatActivity, ChatLog, SharedChatbot};
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    pub limiter: LoginLimiter,
    /// Dokumenten-Chatbot; `None`, wenn nicht aktiviert (siehe `chatbot::NOT_ENABLED`).
    pub chat: Option<SharedChatbot>,
    /// Letzte Chat-Anfragen für das Admin-Panel.
    pub chat_log: ChatLog,
    /// Laufende Chats (der EK-Scan pausiert währenddessen).
    pub chat_activity: ChatActivity,
    /// Kurzer Zeitlimit-Client für den LLM-Zustand im Admin-Panel.
    pub llm_status: Option<Arc<llm::LlmClient>>,
}

impl AppState {
    #[cfg(test)]
    pub fn new(db: PgPool, cfg: Config) -> Self {
        Self::with_chat(db, cfg, None)
    }

    pub fn with_chat(db: PgPool, cfg: Config, chat: Option<SharedChatbot>) -> Self {
        let llm_status = llm::LlmClient::new(cfg.llm_url.clone(), Duration::from_secs(2))
            .ok()
            .map(Arc::new);
        Self {
            db,
            cfg: Arc::new(cfg),
            // 5 Fehlversuche je Nutzername + IP in 10 Minuten
            limiter: LoginLimiter::new(5, Duration::from_secs(600)),
            chat,
            chat_log: ChatLog::default(),
            chat_activity: ChatActivity::default(),
            llm_status,
        }
    }
}
