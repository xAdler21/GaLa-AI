//! Eingabedateien sammeln, PDFium laden, Ground-Truth-Dateien finden.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use pdf::PdfEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    /// Eingescannte Seite als Bild (PNG, JPG, TIFF).
    Image,
}

pub struct Input {
    pub path: PathBuf,
    pub kind: Kind,
}

impl Input {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Von Hand erfasster Referenztext: `<name>.p<Seite>.txt` (Seite 1-basiert) neben dem PDF,
    /// `<name>.txt` neben einem Bild.
    pub fn ground_truth(&self, page: u32) -> Option<String> {
        let stem = self.path.file_stem()?.to_string_lossy();
        let file = match self.kind {
            Kind::Pdf => format!("{stem}.p{}.txt", page + 1),
            Kind::Image => format!("{stem}.txt"),
        };
        std::fs::read_to_string(self.path.with_file_name(file)).ok()
    }
}

fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => Some(Kind::Pdf),
        "png" | "jpg" | "jpeg" | "tif" | "tiff" => Some(Kind::Image),
        _ => None,
    }
}

/// Dateien und Ordner (rekursiv) einsammeln, sortiert für reproduzierbare Läufe.
pub fn collect(paths: &[PathBuf], allow_images: bool) -> anyhow::Result<Vec<Input>> {
    let mut files = Vec::new();
    for path in paths {
        walk(path, &mut files).with_context(|| format!("{} nicht lesbar", path.display()))?;
    }
    files.sort();
    let inputs: Vec<Input> = files
        .into_iter()
        .filter_map(|path| {
            let kind = kind_of(&path)?;
            (kind == Kind::Pdf || allow_images).then_some(Input { path, kind })
        })
        .collect();
    if inputs.is_empty() {
        bail!("keine passenden Eingabedateien gefunden");
    }
    Ok(inputs)
}

fn walk(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            walk(&entry?.path(), out)?;
        }
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
}

/// PDFium aus `dir`, sonst aus `vendor/pdfium/{bin,lib}` (Layout der pdfium-binaries-Archive),
/// sonst aus den Systembibliotheken.
pub fn load_pdf_engine(dir: Option<&Path>) -> anyhow::Result<PdfEngine> {
    let vendored = ["vendor/pdfium/bin", "vendor/pdfium/lib"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir());
    let dir = dir.map(Path::to_path_buf).or(vendored);
    Ok(PdfEngine::load(dir.as_deref())?)
}
