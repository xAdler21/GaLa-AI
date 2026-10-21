use std::path::Path;

use doc_ir::{Page, PageInfo, PageSource, Rect};
use image::RgbImage;
use pdfium_render::prelude::*;

use crate::{CharBox, TextLayerStats, words_from_chars};

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error(
        "PDFium konnte nicht geladen werden – Binary von github.com/bblanchon/pdfium-binaries \
         nach vendor/pdfium legen oder PDFIUM_DIR setzen"
    )]
    Library(#[source] PdfiumError),
    #[error("PDF konnte nicht geöffnet werden")]
    Open(#[source] PdfiumError),
    #[error("Seite {page} konnte nicht verarbeitet werden")]
    Page {
        page: u32,
        #[source]
        source: PdfiumError,
    },
}

/// Prozessweite PDFium-Instanz.
pub struct PdfEngine {
    pdfium: Pdfium,
}

impl PdfEngine {
    /// Lädt PDFium aus `lib_dir`, ohne Angabe aus den Systembibliotheken.
    pub fn load(lib_dir: Option<&Path>) -> Result<Self, PdfError> {
        let bindings = match lib_dir {
            Some(dir) => Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(dir)),
            None => Pdfium::bind_to_system_library(),
        }
        .map_err(PdfError::Library)?;
        Ok(Self {
            pdfium: Pdfium::new(bindings),
        })
    }

    pub fn open(&self, path: &Path) -> Result<PdfDoc<'_>, PdfError> {
        let doc = self
            .pdfium
            .load_pdf_from_file(path, None)
            .map_err(PdfError::Open)?;
        Ok(PdfDoc { doc })
    }
}

pub struct PdfDoc<'a> {
    doc: PdfDocument<'a>,
}

impl<'a> PdfDoc<'a> {
    pub fn page_count(&self) -> u32 {
        self.doc.pages().len().max(0) as u32
    }

    fn page(&self, index: u32) -> Result<PdfPage<'a>, PdfError> {
        self.doc
            .pages()
            .get(index as PdfPageIndex)
            .map_err(|source| PdfError::Page {
                page: index,
                source,
            })
    }

    /// Liest die Textebene einer Seite als Wort-Tokens und bewertet ihre Qualität.
    pub fn text_layer(&self, index: u32) -> Result<(Page, TextLayerStats), PdfError> {
        let page_err = |source| PdfError::Page {
            page: index,
            source,
        };
        let page = self.page(index)?;
        let (width, height) = (page.width().value, page.height().value);
        let text = page.text().map_err(page_err)?;
        let all_chars = text.chars();

        let chars = all_chars.iter().filter_map(|c| {
            let bounds = c.loose_bounds().ok()?;
            let bold =
                c.font_weight().is_some_and(is_bold_weight) || c.font_name().contains("Bold");
            Some(CharBox {
                ch: c.unicode_char().unwrap_or('\u{FFFD}'),
                // PDFium: Ursprung unten links → doc-ir: Ursprung oben links.
                bbox: Rect::new(
                    bounds.left().value,
                    height - bounds.top().value,
                    bounds.right().value,
                    height - bounds.bottom().value,
                ),
                font_size: c.scaled_font_size().value,
                bold,
            })
        });
        let tokens = words_from_chars(chars, index);
        let stats = TextLayerStats::from_tokens(&tokens);

        let page = Page {
            info: PageInfo {
                index,
                width_pt: width,
                height_pt: height,
                source: PageSource::TextLayer,
            },
            tokens,
        };
        Ok((page, stats))
    }

    /// Rendert eine Seite in der gewünschten Auflösung (300 dpi für OCR).
    pub fn render(&self, index: u32, dpi: u32) -> Result<RgbImage, PdfError> {
        let page_err = |source| PdfError::Page {
            page: index,
            source,
        };
        let page = self.page(index)?;
        let width_px = (page.width().value / 72.0 * dpi as f32).round() as Pixels;
        let config = PdfRenderConfig::new().set_target_width(width_px);
        let image = page
            .render_with_config(&config)
            .and_then(|bitmap| bitmap.as_image())
            .map_err(page_err)?;
        Ok(image.into_rgb8())
    }

    pub fn page_size_pt(&self, index: u32) -> Result<(f32, f32), PdfError> {
        let page = self.page(index)?;
        Ok((page.width().value, page.height().value))
    }
}

fn is_bold_weight(weight: PdfFontWeight) -> bool {
    match weight {
        PdfFontWeight::Weight600
        | PdfFontWeight::Weight700Bold
        | PdfFontWeight::Weight800
        | PdfFontWeight::Weight900 => true,
        PdfFontWeight::Custom(w) => w >= 600,
        _ => false,
    }
}
