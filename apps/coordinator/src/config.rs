//! Konfiguration aus Umgebungsvariablen (siehe CLAUDE.md, Tabelle „Umgebungsvariablen“).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    /// Öffentlicher Listener (hinter Caddy).
    pub bind: SocketAddr,
    /// Interner Listener, bedient ausschließlich `/internal/*`.
    pub internal_bind: SocketAddr,
    /// Basis-URL ohne abschließenden Schrägstrich, z. B. `https://gala-coordinator`.
    pub public_url: String,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Schutz für schreibende `/internal`-Routen. Ohne Wert werden alle abgelehnt.
    pub internal_token: Option<String>,
    /// Cookie-Attribut `Secure` (Standard: an).
    pub cookie_secure: bool,
    /// llama-server für den Dokumenten-Chatbot (`GALA_LLM_URL`).
    #[cfg_attr(not(feature = "embed"), allow(dead_code))]
    pub llm_url: String,
    /// Ablage des Embedding-Modells (`GALA_EMBED_CACHE_DIR`, nur mit Feature `embed`).
    #[cfg_attr(not(feature = "embed"), allow(dead_code))]
    pub embed_cache_dir: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Wie [`Config::from_env`], aber mit austauschbarer Quelle (für Tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let nonempty = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let bind = nonempty("GALA_BIND").unwrap_or_else(|| "127.0.0.1:8080".into());
        let internal_bind = nonempty("GALA_INTERNAL_BIND").unwrap_or_else(|| "0.0.0.0:8081".into());
        let public_url =
            nonempty("GALA_PUBLIC_URL").unwrap_or_else(|| "http://localhost:8080".into());
        Ok(Self {
            bind: bind
                .parse()
                .with_context(|| format!("GALA_BIND ungültig: {bind}"))?,
            internal_bind: internal_bind
                .parse()
                .with_context(|| format!("GALA_INTERNAL_BIND ungültig: {internal_bind}"))?,
            public_url: public_url.trim_end_matches('/').to_string(),
            data_dir: nonempty("GALA_DATA_DIR")
                .unwrap_or_else(|| "./data".into())
                .into(),
            state_dir: nonempty("GALA_STATE_DIR")
                .unwrap_or_else(|| "./state".into())
                .into(),
            internal_token: nonempty("GALA_INTERNAL_TOKEN"),
            llm_url: nonempty("GALA_LLM_URL")
                .unwrap_or_else(|| "http://gala-llm:8080".into())
                .trim_end_matches('/')
                .to_string(),
            embed_cache_dir: nonempty("GALA_EMBED_CACHE_DIR").map(PathBuf::from),
            cookie_secure: match nonempty("GALA_COOKIE_SECURE") {
                None => true,
                Some(v) => !matches!(
                    v.to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                ),
            },
        })
    }

    pub fn releases_dir(&self) -> PathBuf {
        self.data_dir.join("releases")
    }

    pub fn ca_cert_path(&self) -> PathBuf {
        self.data_dir.join("ca.crt")
    }

    /// Host(:Port) aus `public_url`, für den Origin-Vergleich.
    pub fn public_authority(&self) -> Option<&str> {
        let rest = self.public_url.split_once("://")?.1;
        Some(rest.split('/').next().unwrap_or(rest))
    }

    /// Konfiguration für Tests ohne Umgebung.
    #[cfg(test)]
    pub fn for_tests(dir: &std::path::Path) -> Self {
        Self {
            bind: "127.0.0.1:0".parse().unwrap(),
            internal_bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "https://gala.test".into(),
            data_dir: dir.join("data"),
            state_dir: dir.join("state"),
            internal_token: Some("geheim".into()),
            cookie_secure: false,
            llm_url: "http://gala-llm:8080".into(),
            embed_cache_dir: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::from_lookup(|_| None).unwrap();
        assert_eq!(c.bind.to_string(), "127.0.0.1:8080");
        assert_eq!(c.internal_bind.to_string(), "0.0.0.0:8081");
        assert!(c.cookie_secure);
        assert!(c.internal_token.is_none());
        assert_eq!(c.llm_url, "http://gala-llm:8080");
    }

    #[test]
    fn overrides_and_trailing_slash() {
        let c = Config::from_lookup(|k| match k {
            "GALA_PUBLIC_URL" => Some("https://gala-coordinator/".into()),
            "GALA_COOKIE_SECURE" => Some("false".into()),
            "GALA_LLM_URL" => Some("http://llm.test:9000/".into()),
            "GALA_INTERNAL_TOKEN" => Some("  ".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(c.public_url, "https://gala-coordinator");
        assert_eq!(c.public_authority(), Some("gala-coordinator"));
        assert_eq!(c.llm_url, "http://llm.test:9000");
        assert!(!c.cookie_secure);
        assert!(c.internal_token.is_none());
    }

    #[test]
    fn invalid_bind_is_error() {
        assert!(Config::from_lookup(|k| (k == "GALA_BIND").then(|| "kaputt".into())).is_err());
    }
}
