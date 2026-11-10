//! Desktop-Updates im dynamischen Format des Tauri-2-Updaters.

use chrono::{DateTime, Utc};
use db::releases::Release;
use semver::Version;
use serde::Serialize;

/// Antwort des Updaters (`200`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateResponse {
    pub version: String,
    pub notes: String,
    pub pub_date: Option<DateTime<Utc>>,
    pub url: String,
    pub signature: String,
}

/// Versionsangabe tolerant parsen (führendes `v` erlaubt).
pub fn parse_version(s: &str) -> Option<Version> {
    Version::parse(s.trim().trim_start_matches('v')).ok()
}

/// Neuestes Release nach SemVer (nicht-parsbare Versionen werden ignoriert).
pub fn newest(releases: &[Release]) -> Option<(Version, &Release)> {
    releases
        .iter()
        .filter_map(|r| Some((parse_version(&r.version)?, r)))
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// Download-URL einer Release-Datei.
pub fn release_url(public_url: &str, version: &str, file: &str) -> String {
    format!(
        "{}/releases/{version}/{file}",
        public_url.trim_end_matches('/')
    )
}

/// Entscheidung des Updaters: `Some`, wenn ein neueres Release als `current` existiert.
pub fn decide(releases: &[Release], current: &Version, public_url: &str) -> Option<UpdateResponse> {
    let (version, rel) = newest(releases)?;
    (version > *current).then(|| UpdateResponse {
        version: version.to_string(),
        notes: rel.notes.clone(),
        pub_date: rel.published_at,
        url: release_url(public_url, &rel.version, &rel.file_name),
        signature: rel.signature.clone(),
    })
}

#[cfg(test)]
pub fn test_release(version: &str) -> Release {
    Release {
        id: uuid::Uuid::nil(),
        version: version.into(),
        platform: "windows-x86_64".into(),
        file_name: format!("GaLa-AI_{version}_x64-setup.exe"),
        signature: "SIG".into(),
        notes: "Notizen".into(),
        published: true,
        created_at: Utc::now(),
        published_at: Some(Utc::now()),
        created_by: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn no_releases_means_no_update() {
        assert!(decide(&[], &v("0.1.0"), "https://x").is_none());
    }

    #[test]
    fn newer_release_is_offered() {
        let rels = [
            test_release("0.2.0"),
            test_release("0.10.0"),
            test_release("0.9.1"),
        ];
        let r = decide(&rels, &v("0.9.0"), "https://gala/").unwrap();
        assert_eq!(r.version, "0.10.0");
        assert_eq!(
            r.url,
            "https://gala/releases/0.10.0/GaLa-AI_0.10.0_x64-setup.exe"
        );
        assert_eq!(r.signature, "SIG");
    }

    #[test]
    fn same_or_older_is_204() {
        let rels = [test_release("0.2.0")];
        assert!(decide(&rels, &v("0.2.0"), "https://x").is_none());
        assert!(decide(&rels, &v("1.0.0"), "https://x").is_none());
    }

    #[test]
    fn prerelease_is_older_than_release() {
        let rels = [test_release("1.0.0")];
        assert!(decide(&rels, &v("1.0.0-beta.1"), "https://x").is_some());
    }

    #[test]
    fn version_parsing_accepts_v_prefix() {
        assert_eq!(parse_version("v1.2.3"), Some(v("1.2.3")));
        assert_eq!(parse_version("kaputt"), None);
    }
}
