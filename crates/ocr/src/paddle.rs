use std::path::PathBuf;

use doc_ir::{Page, PageInfo, PageSource, Rect};
use oar_ocr::core::config::{OrtExecutionProvider, OrtSessionConfig};
use oar_ocr::oarocr::{OAROCR, OAROCRBuilder};

use crate::{OcrEngine, OcrError, PageImage, line_to_tokens};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Cuda { device_id: i32 },
}

impl std::fmt::Display for Device {
    /// `cpu` oder `cuda:N`, wie auf der Kommandozeile.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Device::Cpu => f.write_str("cpu"),
            Device::Cuda { device_id } => write!(f, "cuda:{device_id}"),
        }
    }
}

/// Modellkombinationen für den M0-Vergleich. Die Dateinamen stehen im Modellregister von
/// oar-ocr; mit dem Feature `auto-download` werden sie beim ersten Lauf nach `$OAR_HOME` geladen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// PP-OCRv5 mobile Detektion + lateinische Erkennung (Umlaute, ß). Klein und schnell.
    V5Latin,
    /// PP-OCRv5 server Detektion + lateinische Erkennung. Bessere Boxen bei schlechten Scans.
    V5ServerLatin,
    /// PP-OCRv6 small, mehrsprachiges Wörterbuch.
    V6Small,
    /// PP-OCRv6 medium, mehrsprachiges Wörterbuch.
    V6Medium,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::V5Latin,
        Preset::V5ServerLatin,
        Preset::V6Small,
        Preset::V6Medium,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Preset::V5Latin => "v5-latin",
            Preset::V5ServerLatin => "v5-server-latin",
            Preset::V6Small => "v6-small",
            Preset::V6Medium => "v6-medium",
        }
    }

    /// (Detektion, Erkennung, Wörterbuch)
    fn files(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Preset::V5Latin => (
                "pp-ocrv5_mobile_det.onnx",
                "latin_pp-ocrv5_mobile_rec.onnx",
                "ppocrv5_latin_dict.txt",
            ),
            Preset::V5ServerLatin => (
                "pp-ocrv5_server_det.onnx",
                "latin_pp-ocrv5_mobile_rec.onnx",
                "ppocrv5_latin_dict.txt",
            ),
            Preset::V6Small => (
                "pp-ocrv6_small_det.onnx",
                "pp-ocrv6_small_rec.onnx",
                "ppocrv6_dict.txt",
            ),
            Preset::V6Medium => (
                "pp-ocrv6_medium_det.onnx",
                "pp-ocrv6_medium_rec.onnx",
                "ppocrv6_dict.txt",
            ),
        }
    }
}

impl std::str::FromStr for Preset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Preset::ALL
            .into_iter()
            .find(|p| p.name() == s)
            .ok_or_else(|| {
                let names: Vec<&str> = Preset::ALL.iter().map(|p| p.name()).collect();
                format!(
                    "unbekanntes OCR-Preset {s:?}, erlaubt: {}",
                    names.join(", ")
                )
            })
    }
}

/// PaddleOCR (PP-OCR) über oar-ocr / ONNX Runtime.
pub struct PaddleOcr {
    name: String,
    ocr: OAROCR,
}

impl PaddleOcr {
    /// `model_dir`: Verzeichnis mit den Modelldateien. Ohne Angabe werden die Namen über den
    /// oar-ocr-Cache aufgelöst (Feature `auto-download`).
    pub fn new(
        preset: Preset,
        device: Device,
        model_dir: Option<PathBuf>,
    ) -> Result<Self, OcrError> {
        let (det, rec, dict) = preset.files();
        let path = |file: &str| match &model_dir {
            Some(dir) => dir.join(file),
            None => PathBuf::from(file),
        };

        let mut builder =
            OAROCRBuilder::new(path(det), path(rec), path(dict)).return_word_box(true);
        if let Device::Cuda { device_id } = device {
            // oar-ocr registriert den CUDA-Provider nachsichtig: lädt er nicht, rechnet die
            // Runtime kommentarlos auf der CPU. Deshalb vorher streng prüfen.
            probe_cuda(device_id)?;
            builder = builder.ort_session(OrtSessionConfig::new().with_execution_providers(vec![
                OrtExecutionProvider::CUDA {
                    device_id: Some(device_id),
                    gpu_mem_limit: None,
                    arena_extend_strategy: None,
                    cudnn_conv_algo_search: None,
                    cudnn_conv_use_max_workspace: None,
                },
                OrtExecutionProvider::CPU,
            ]));
        }

        let ocr = builder.build().map_err(|e| OcrError::Init(e.to_string()))?;
        Ok(Self {
            name: format!("paddle/{}@{device}", preset.name()),
            ocr,
        })
    }
}

/// Prüft, ob sich der CUDA-Provider der ONNX Runtime registrieren lässt (Provider-Bibliothek
/// und ihre CUDA-Bibliotheken ladbar, Gerät vorhanden). Schlägt das fehl, gibt es einen Fehler
/// statt eines stillen Rückfalls auf die CPU.
///
/// Fehler, die erst beim Rechnen auftreten (z. B. fehlende Kernel für eine alte GPU), findet
/// diese Prüfung nicht; dafür gibt es `deploy/ocr/gpu-test.sh`.
#[cfg(feature = "cuda")]
fn probe_cuda(device_id: i32) -> Result<(), OcrError> {
    let fail = |e: String| {
        OcrError::Init(format!(
            "CUDA-Provider für cuda:{device_id} nicht ladbar: {e} \
             (ohne GPU weiterarbeiten: Gerät `cpu` wählen, im Worker GALA_OCR_DEVICE=cpu)"
        ))
    };
    let provider = ort::ep::CUDA::default()
        .with_device_id(device_id)
        .build()
        .error_on_failure();
    ort::session::Session::builder()
        .map_err(|e| fail(e.to_string()))?
        .with_execution_providers([provider])
        .map_err(|e| fail(e.to_string()))?;
    Ok(())
}

#[cfg(not(feature = "cuda"))]
fn probe_cuda(_device_id: i32) -> Result<(), OcrError> {
    Err(OcrError::Init(
        "GPU angefordert, aber ohne Feature `cuda` gebaut".into(),
    ))
}

impl OcrEngine for PaddleOcr {
    fn name(&self) -> &str {
        &self.name
    }

    fn recognize(&self, pages: Vec<PageImage>) -> Result<Vec<Page>, OcrError> {
        if pages.is_empty() {
            return Ok(Vec::new());
        }
        let meta: Vec<(u32, u32, u32, u32)> = pages
            .iter()
            .map(|p| (p.page, p.dpi, p.image.width(), p.image.height()))
            .collect();
        let results = self
            .ocr
            .predict(pages.into_iter().map(|p| p.image).collect())
            .map_err(|e| OcrError::Inference(e.to_string()))?;

        Ok(results
            .iter()
            .zip(meta)
            .map(|(result, (page, dpi, w_px, h_px))| {
                let scale = 72.0 / dpi as f32;
                let to_pt = |b: &oar_ocr::processors::BoundingBox| {
                    Rect::new(b.x_min(), b.y_min(), b.x_max(), b.y_max()).scale(scale)
                };
                let tokens = result
                    .text_regions
                    .iter()
                    .filter_map(|region| Some((region, region.text.as_deref()?)))
                    .flat_map(|(region, text)| {
                        let char_boxes: Option<Vec<Rect>> = region
                            .word_boxes
                            .as_ref()
                            .map(|boxes| boxes.iter().map(to_pt).collect());
                        line_to_tokens(
                            text,
                            to_pt(&region.bounding_box),
                            char_boxes.as_deref(),
                            region.confidence.unwrap_or(0.0),
                            page,
                        )
                    })
                    .collect();
                Page {
                    info: PageInfo {
                        index: page,
                        width_pt: w_px as f32 * scale,
                        height_pt: h_px as f32 * scale,
                        source: PageSource::Ocr { dpi },
                    },
                    tokens,
                }
            })
            .collect())
    }
}
