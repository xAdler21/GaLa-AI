//! Netzlaufwerk durchlaufen: Dateien erkennen, Temp-/versteckte Dateien überspringen,
//! SHA-256 streamend berechnen und gegen die `document`-Zeilen abgleichen.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::IngestError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocKind {
    Pdf,
    Xlsx,
    Gaeb,
    Text,
}

impl DocKind {
    /// Wert in `document.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Pdf => "pdf",
            DocKind::Xlsx => "xlsx",
            DocKind::Gaeb => "gaeb",
            DocKind::Text => "text",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pdf" => DocKind::Pdf,
            "xlsx" => DocKind::Xlsx,
            "gaeb" => DocKind::Gaeb,
            "text" => DocKind::Text,
            _ => return None,
        })
    }
}

/// Dokumentart anhand der Dateiendung.
pub fn detect_kind(path: &Path) -> Option<DocKind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    Some(match ext.as_str() {
        "pdf" => DocKind::Pdf,
        "xlsx" | "xls" | "xlsm" => DocKind::Xlsx,
        "x81" | "x82" | "x83" | "x84" | "x85" | "x86" | "d81" | "d82" | "d83" | "d84" | "d85"
        | "d86" => DocKind::Gaeb,
        "txt" | "md" => DocKind::Text,
        _ => return None,
    })
}

/// Dateien, die nie indiziert werden: versteckt, Office-Sperrdateien, Thumbnails, Temp.
pub fn is_ignored_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with('.')
        || name.starts_with("~$")
        || name.ends_with('~')
        || lower == "thumbs.db"
        || lower == "desktop.ini"
        || lower.ends_with(".tmp")
}

/// Verzeichnisse, in die nicht abgestiegen wird (versteckt, Papierkörbe, NAS-Metadaten).
pub fn is_ignored_dir(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with('.')
        || lower == "$recycle.bin"
        || lower == "#recycle"
        || lower == "@eadir"
        || lower == "system volume information"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    /// Relativ zur Wurzel, immer mit `/`.
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub size: u64,
    pub mtime: DateTime<Utc>,
    pub kind: DocKind,
}

/// Durchläuft `root` rekursiv (ohne Symlinks zu folgen). Einzelne unlesbare Einträge werden
/// protokolliert und übersprungen; ist die Wurzel selbst nicht lesbar, ist das ein Fehler.
pub fn scan_dir(root: &Path) -> Result<Vec<ScannedFile>, IngestError> {
    std::fs::read_dir(root).map_err(|source| IngestError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    let walker = WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_dir()
                || !is_ignored_dir(&e.file_name().to_string_lossy())
        });

    let mut files = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(%err, "Eintrag beim Scannen übersprungen");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if is_ignored_file(&name) {
            continue;
        }
        let Some(kind) = detect_kind(entry.path()) else {
            continue;
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(%err, path = %entry.path().display(), "Metadaten nicht lesbar");
                continue;
            }
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel_path = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        files.push(ScannedFile {
            rel_path,
            abs_path: entry.path().to_path_buf(),
            size: meta.len(),
            mtime: DateTime::<Utc>::from(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
            kind,
        });
    }
    Ok(files)
}

/// SHA-256 einer Datei, gestreamt (Hex, klein).
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Stand einer Datei laut `document`-Tabelle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownDoc {
    pub rel_path: String,
    pub size: i64,
    pub mtime: DateTime<Utc>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Nicht in der Datenbank.
    pub new: Vec<ScannedFile>,
    /// Größe oder Änderungszeit weichen ab (Inhalt per Hash prüfen).
    pub changed: Vec<ScannedFile>,
    /// In der Datenbank, aber nicht mehr auf dem Laufwerk.
    pub deleted: Vec<String>,
}

/// Vergleicht Scan und Datenbank. Die Änderungszeit wird auf Mikrosekunden verglichen
/// (Genauigkeit von `timestamptz`).
pub fn diff(scanned: Vec<ScannedFile>, known: &[KnownDoc]) -> Diff {
    let known_by_path: HashMap<&str, &KnownDoc> =
        known.iter().map(|k| (k.rel_path.as_str(), k)).collect();
    let seen: HashSet<String> = scanned.iter().map(|f| f.rel_path.clone()).collect();

    let mut out = Diff::default();
    for file in scanned {
        match known_by_path.get(file.rel_path.as_str()) {
            None => out.new.push(file),
            Some(k) => {
                if k.size != file.size as i64
                    || k.mtime.timestamp_micros() != file.mtime.timestamp_micros()
                {
                    out.changed.push(file);
                }
            }
        }
    }
    out.deleted = known
        .iter()
        .filter(|k| !seen.contains(&k.rel_path))
        .map(|k| k.rel_path.clone())
        .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn kinds_by_extension() {
        let k = |p: &str| detect_kind(Path::new(p));
        assert_eq!(k("a/B.PDF"), Some(DocKind::Pdf));
        assert_eq!(k("a.xlsx"), Some(DocKind::Xlsx));
        assert_eq!(k("a.XLS"), Some(DocKind::Xlsx));
        assert_eq!(k("lv.x83"), Some(DocKind::Gaeb));
        assert_eq!(k("lv.X84"), Some(DocKind::Gaeb));
        assert_eq!(k("lv.d83"), Some(DocKind::Gaeb));
        assert_eq!(k("n.txt"), Some(DocKind::Text));
        assert_eq!(k("n.md"), Some(DocKind::Text));
        assert_eq!(k("bild.jpg"), None);
        assert_eq!(k("ohne_endung"), None);
        for kind in [DocKind::Pdf, DocKind::Xlsx, DocKind::Gaeb, DocKind::Text] {
            assert_eq!(DocKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn ignore_filters() {
        for n in [
            "~$Angebot.xlsx",
            ".hidden.pdf",
            "Thumbs.db",
            "thumbs.db",
            "desktop.ini",
            "x.tmp",
            "backup.txt~",
        ] {
            assert!(is_ignored_file(n), "{n}");
        }
        for n in ["Angebot.pdf", "Kita~1.pdf", "tmp.pdf"] {
            assert!(!is_ignored_file(n), "{n}");
        }
        for n in [
            ".git",
            "$RECYCLE.BIN",
            "#recycle",
            "@eaDir",
            "System Volume Information",
        ] {
            assert!(is_ignored_dir(n), "{n}");
        }
        assert!(!is_ignored_dir("2019"));
    }

    #[test]
    fn scans_tree_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Angebote/2019")).unwrap();
        fs::create_dir_all(root.join("@eaDir/x")).unwrap();
        fs::create_dir_all(root.join(".versteckt")).unwrap();
        fs::write(root.join("Angebote/2019/Kita.pdf"), b"%PDF").unwrap();
        fs::write(root.join("Angebote/2019/~$Kita.xlsx"), b"x").unwrap();
        fs::write(root.join("Angebote/Preise.xlsx"), b"x").unwrap();
        fs::write(root.join("Angebote/Thumbs.db"), b"x").unwrap();
        fs::write(root.join("Angebote/foto.jpg"), b"x").unwrap();
        fs::write(root.join("@eaDir/x/a.pdf"), b"x").unwrap();
        fs::write(root.join(".versteckt/b.pdf"), b"x").unwrap();
        fs::write(root.join("notiz.TXT"), b"hallo").unwrap();

        let files = scan_dir(root).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "Angebote/2019/Kita.pdf",
                "Angebote/Preise.xlsx",
                "notiz.TXT"
            ]
        );
        assert_eq!(files[2].size, 5);
        assert_eq!(files[2].kind, DocKind::Text);
    }

    #[test]
    fn missing_root_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan_dir(&dir.path().join("gibtsnicht")).is_err());
    }

    #[test]
    fn sha256_streams_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // größer als der Puffer
        fs::write(&p, vec![b'a'; (1 << 20) + 17]).unwrap();
        assert_eq!(sha256_file(&p).unwrap().len(), 64);
    }

    fn sf(path: &str, size: u64, secs: i64) -> ScannedFile {
        ScannedFile {
            rel_path: path.into(),
            abs_path: PathBuf::from(path),
            size,
            mtime: DateTime::from_timestamp(secs, 0).unwrap(),
            kind: DocKind::Pdf,
        }
    }

    fn kd(path: &str, size: i64, secs: i64) -> KnownDoc {
        KnownDoc {
            rel_path: path.into(),
            size,
            mtime: DateTime::from_timestamp(secs, 0).unwrap(),
        }
    }

    #[test]
    fn diff_detects_new_changed_deleted() {
        let scanned = vec![
            sf("a.pdf", 10, 100),
            sf("b.pdf", 20, 200),
            sf("c.pdf", 31, 300),
            sf("d.pdf", 40, 401),
        ];
        let known = vec![
            kd("a.pdf", 10, 100),
            kd("c.pdf", 30, 300),
            kd("d.pdf", 40, 400),
            kd("gone.pdf", 1, 1),
        ];
        let d = diff(scanned, &known);
        assert_eq!(
            d.new
                .iter()
                .map(|f| f.rel_path.as_str())
                .collect::<Vec<_>>(),
            ["b.pdf"]
        );
        assert_eq!(
            d.changed
                .iter()
                .map(|f| f.rel_path.as_str())
                .collect::<Vec<_>>(),
            ["c.pdf", "d.pdf"]
        );
        assert_eq!(d.deleted, ["gone.pdf"]);
    }

    #[test]
    fn diff_empty() {
        assert_eq!(diff(vec![], &[]), Diff::default());
    }
}
