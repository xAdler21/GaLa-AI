//! Gemeinsamer Anwendungszustand.

use std::sync::Arc;
use std::time::Duration;

use db::PgPool;

use crate::auth::LoginLimiter;
use crate::chatbot::SharedChatbot;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    pub limiter: LoginLimiter,
    /// Dokumenten-Chatbot; `None`, wenn nicht aktiviert (siehe `chatbot::NOT_ENABLED`).
    pub chat: Option<SharedChatbot>,
}

impl AppState {
    #[cfg(test)]
    pub fn new(db: PgPool, cfg: Config) -> Self {
        Self::with_chat(db, cfg, None)
    }

    pub fn with_chat(db: PgPool, cfg: Config, chat: Option<SharedChatbot>) -> Self {
        Self {
            db,
            cfg: Arc::new(cfg),
            // 5 Fehlversuche je Nutzername + IP in 10 Minuten
            limiter: LoginLimiter::new(5, Duration::from_secs(600)),
            chat,
        }
    }
}
