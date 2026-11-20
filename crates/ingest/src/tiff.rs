//! Gescannte TIFF-Dateien (oft mehrseitig, Fax-Kompression). Jede Seite wird wie eine
//! PDF-Seite ohne Textebene per OCR gelesen.
//!
//! `image` dekodiert nur das erste Bild einer TIFF-Datei. Für Seite N wird deshalb im Kopf
//! der Zeiger auf das erste Bildverzeichnis (IFD) auf das N-te gesetzt; den Rest (Farbmodelle,
//! 1-Bit-Fax, LZW, JPEG) erledigt `image`.

use std::path::Path;

use image::RgbImage;

use crate::IngestError;
use crate::extract::read_bytes;

/// Anzahl der Seiten (Bildverzeichnisse).
pub fn tiff_page_count(path: &Path) -> Result<u32, IngestError> {
    let bytes = read_bytes(path)?;
    let n = ifd_offsets(&bytes).ok_or_else(|| not_tiff(path))?.len();
    Ok(n as u32)
}

/// Seite `index` (0-basiert) als RGB-Bild.
pub fn tiff_page(path: &Path, index: u32) -> Result<RgbImage, IngestError> {
    decode_page(read_bytes(path)?, index).map_err(|message| IngestError::Extract {
        path: path.to_path_buf(),
        message,
    })
}

fn not_tiff(path: &Path) -> IngestError {
    IngestError::Extract {
        path: path.to_path_buf(),
        message: "keine lesbare TIFF-Datei".into(),
    }
}

fn decode_page(mut bytes: Vec<u8>, index: u32) -> Result<RgbImage, String> {
    let offsets = ifd_offsets(&bytes).ok_or("keine lesbare TIFF-Datei")?;
    let off = *offsets
        .get(index as usize)
        .ok_or_else(|| format!("Seite {} fehlt (nur {} Seiten)", index + 1, offsets.len()))?;
    let le = bytes[0] == b'I';
    bytes[4..8].copy_from_slice(&if le {
        off.to_le_bytes()
    } else {
        off.to_be_bytes()
    });
    let decoder = image::codecs::tiff::TiffDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| e.to_string())?;
    let img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    Ok(img.to_rgb8())
}

/// Offsets der Bildverzeichnisse, eins je Seite. Nur klassisches TIFF (kein BigTIFF).
fn ifd_offsets(b: &[u8]) -> Option<Vec<u32>> {
    let le = match b.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| {
        let s: [u8; 2] = b.get(o..o + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(s)
        } else {
            u16::from_be_bytes(s)
        })
    };
    let u32_at = |o: usize| {
        let s: [u8; 4] = b.get(o..o + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let mut out = Vec::new();
    let mut off = u32_at(4)?;
    // Obergrenze und Wiederholungsprüfung gegen kaputte Dateien mit IFD-Schleife.
    while off != 0 && out.len() < 1000 && !out.contains(&off) {
        out.push(off);
        let entries = u16_at(off as usize)? as usize;
        off = u32_at(off as usize + 2 + 12 * entries)?;
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::encoder::{TiffEncoder, colortype};

    /// Zwei Graustufen-Seiten unterschiedlicher Größe; Seite 2 ist weiß.
    fn two_pages() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut buf).unwrap();
        enc.write_image::<colortype::Gray8>(4, 3, &[0u8; 12])
            .unwrap();
        enc.write_image::<colortype::Gray8>(5, 2, &[255u8; 10])
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn counts_and_decodes_each_page() {
        let bytes = two_pages();
        assert_eq!(ifd_offsets(&bytes).map(|o| o.len()), Some(2));
        let p1 = decode_page(bytes.clone(), 0).unwrap();
        assert_eq!(p1.dimensions(), (4, 3));
        assert_eq!(p1.get_pixel(0, 0).0, [0, 0, 0]);
        let p2 = decode_page(bytes.clone(), 1).unwrap();
        assert_eq!(p2.dimensions(), (5, 2));
        assert_eq!(p2.get_pixel(4, 1).0, [255, 255, 255]);
        assert!(decode_page(bytes, 2).is_err());
    }

    #[test]
    fn rejects_non_tiff() {
        assert_eq!(ifd_offsets(b"%PDF-1.4"), None);
        assert_eq!(ifd_offsets(b"II*\0"), None);
    }
}
