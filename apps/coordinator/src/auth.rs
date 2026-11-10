//! Passwörter (argon2), Tokens (SHA-256-Hash in der DB), Login-Rate-Limit und Client-IP.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

/// Mindestlänge neuer Passwörter.
pub const MIN_PASSWORD_LEN: usize = 8;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("Passwort-Hash fehlgeschlagen: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

/// Scheinbar-Hash, damit Login-Versuche für unbekannte Nutzer genauso lange dauern.
pub fn dummy_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| hash_password("dummy-passwort-fuer-timing").unwrap_or_default())
}

/// Zufälliges 32-Byte-Token, hex-kodiert (64 Zeichen).
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes[..]);
    hex::encode(bytes)
}

/// SHA-256 des Tokens (so liegt es in `session.token_hash`).
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Token aus `Authorization: Bearer <token>`.
pub fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let v = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim())
}

/// Wert eines Cookies aus dem `Cookie`-Header.
pub fn cookie_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// IP des Clients. Kommt die Verbindung von Loopback (Caddy), zählt `X-Forwarded-For`.
pub fn client_ip(peer: Option<SocketAddr>, headers: &axum::http::HeaderMap) -> String {
    let peer_ip = peer.map(|p| p.ip());
    if peer_ip.is_none_or(|ip| ip.is_loopback())
        && let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = xff
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return first.to_string();
    }
    peer_ip.map_or_else(|| "unbekannt".into(), |ip| ip.to_string())
}

/// Extractor für die Client-IP (nie fehlerhaft).
pub struct ClientIp(pub String);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        Ok(Self(client_ip(peer, &parts.headers)))
    }
}

/// Einfache Begrenzung fehlgeschlagener Logins je Nutzername + IP (nur im Speicher).
#[derive(Clone)]
pub struct LoginLimiter {
    inner: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    max_failures: usize,
    window: Duration,
}

impl LoginLimiter {
    pub fn new(max_failures: usize, window: Duration) -> Self {
        Self {
            inner: Arc::default(),
            max_failures,
            window,
        }
    }

    fn key(username: &str, ip: &str) -> String {
        format!("{}|{ip}", username.to_lowercase())
    }

    /// Ist der Schlüssel aktuell gesperrt? Liefert die Wartezeit.
    pub fn blocked(&self, username: &str, ip: &str) -> Option<Duration> {
        self.blocked_at(username, ip, Instant::now())
    }

    fn blocked_at(&self, username: &str, ip: &str, now: Instant) -> Option<Duration> {
        let mut map = self.inner.lock().ok()?;
        let list = map.get_mut(&Self::key(username, ip))?;
        list.retain(|t| now.duration_since(*t) < self.window);
        if list.len() >= self.max_failures {
            let oldest = *list.first()?;
            Some(self.window.saturating_sub(now.duration_since(oldest)))
        } else {
            None
        }
    }

    pub fn record_failure(&self, username: &str, ip: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.entry(Self::key(username, ip))
                .or_default()
                .push(Instant::now());
        }
    }

    pub fn clear(&self, username: &str, ip: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(&Self::key(username, ip));
        }
    }

    /// Entfernt abgelaufene Einträge (periodisch aufrufen).
    pub fn prune(&self) {
        let now = Instant::now();
        if let Ok(mut map) = self.inner.lock() {
            map.retain(|_, list| {
                list.retain(|t| now.duration_since(*t) < self.window);
                !list.is_empty()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn token_hash_is_sha256_hex_of_token() {
        // SHA-256("abc")
        assert_eq!(
            hex::encode(hash_token("abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_token("x"), hash_token("x"));
        assert_ne!(hash_token("x"), hash_token("y"));
    }

    #[test]
    fn tokens_are_random_and_64_hex_chars() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("geheim123").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("geheim123", &h));
        assert!(!verify_password("falsch", &h));
        assert!(!verify_password("x", "kein-hash"));
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc123"),
        );
        assert_eq!(bearer_token(&h), Some("abc123"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer_token(&h), None);
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; gala_admin=tok; b=2"),
        );
        assert_eq!(cookie_value(&h, "gala_admin"), Some("tok"));
        assert_eq!(cookie_value(&h, "x"), None);
    }

    #[test]
    fn forwarded_for_only_from_loopback() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.5, 127.0.0.1"),
        );
        let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let ext: SocketAddr = "192.168.1.9:5000".parse().unwrap();
        assert_eq!(client_ip(Some(lo), &h), "10.0.0.5");
        assert_eq!(client_ip(Some(ext), &h), "192.168.1.9");
    }

    #[test]
    fn limiter_blocks_after_max_failures_and_clears() {
        let l = LoginLimiter::new(3, Duration::from_secs(60));
        for _ in 0..2 {
            l.record_failure("Anna", "1.2.3.4");
        }
        assert!(l.blocked("anna", "1.2.3.4").is_none());
        l.record_failure("anna", "1.2.3.4");
        assert!(l.blocked("ANNA", "1.2.3.4").is_some());
        assert!(l.blocked("anna", "9.9.9.9").is_none());
        l.clear("anna", "1.2.3.4");
        assert!(l.blocked("anna", "1.2.3.4").is_none());
    }

    #[test]
    fn limiter_window_expires() {
        let l = LoginLimiter::new(1, Duration::from_secs(10));
        l.record_failure("a", "ip");
        assert!(l.blocked_at("a", "ip", Instant::now()).is_some());
        assert!(
            l.blocked_at("a", "ip", Instant::now() + Duration::from_secs(11))
                .is_none()
        );
    }
}
