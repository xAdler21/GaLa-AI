//! Gescannte TIFF-Dateien (oft mehrseitig, Fax-Kompression). Jede Seite wird wie eine
//! PDF-Seite ohne Textebene per OCR gelesen.
//!
//! `image` dekodiert nur das erste Bild einer TIFF-Datei. Für Seite N wird deshalb im Kopf
//! der Zeiger auf das erste Bildverzeichnis (IFD) auf das N-te gesetzt; den Rest (Farbmodelle,
//! LZW, Deflate, JPEG) erledigt `image`. Fax (CCITT G4) dekodieren wir selbst mit `fax`, weil
//! `tiff` an Seiten ohne die weißen Schlusszeilen scheitert. Außerdem: umgekehrte Bitfolge
//! (FillOrder 2, ignoriert `tiff`) und 204×98 dpi (Normalauflösung, halbe Höhe) ausgleichen.
//!
//! shortcut: alte JPEG-Kompression (6, z. B. SCAN0252_000.tif), G3-Fax und gekachelte Bilder
//! mit FillOrder 2 werden nicht unterstützt; die Seite scheitert mit Meldung. Erst angehen, wenn
//! solche Dateien wichtig werden (dann z. B. libtiff).

use std::path::Path;

use image::{ImageDecoder, RgbImage};

use crate::IngestError;
use crate::extract::read_bytes;

const TAG_WIDTH: u16 = 256;
const TAG_HEIGHT: u16 = 257;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_FILL_ORDER: u16 = 266;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_X_RESOLUTION: u16 = 282;
const TAG_Y_RESOLUTION: u16 = 283;

/// Speichergrenze beim Dekodieren. Große Pläne (A1 mit 300 dpi, RGBA) brauchen um 300 MB;
/// die Voreinstellung von `image`/`tiff` (256 MB) ist dafür zu klein.
const MAX_ALLOC: u64 = 2 << 30;

/// Anzahl der Seiten (Bildverzeichnisse).
pub fn tiff_page_count(path: &Path) -> Result<u32, IngestError> {
    let bytes = read_bytes(path)?;
    let n = Tiff::new(&bytes)
        .and_then(|t| t.ifd_offsets())
        .ok_or_else(|| not_tiff(path))?
        .len();
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
    const NOT_TIFF: &str = "keine lesbare TIFF-Datei";
    let t = Tiff::new(&bytes).ok_or(NOT_TIFF)?;
    let offsets = t.ifd_offsets().ok_or(NOT_TIFF)?;
    let ifd = *offsets
        .get(index as usize)
        .ok_or_else(|| format!("Seite {} fehlt (nur {} Seiten)", index + 1, offsets.len()))?;
    let first = |tag| t.tag(ifd, tag).and_then(|v| v.first().copied());
    let reversed = first(TAG_FILL_ORDER) == Some(2);
    let strips: Vec<(usize, usize)> = match (
        t.tag(ifd, TAG_STRIP_OFFSETS),
        t.tag(ifd, TAG_STRIP_BYTE_COUNTS),
    ) {
        (Some(offs), Some(lens)) => offs
            .iter()
            .zip(&lens)
            .map(|(&o, &l)| (o as usize, l as usize))
            .collect(),
        _ => Vec::new(),
    };
    // Breite/Höhe eines Pixels; Normalauflösung beim Fax ist 204×98 dpi.
    let stretch = match (
        t.rational(ifd, TAG_X_RESOLUTION),
        t.rational(ifd, TAG_Y_RESOLUTION),
    ) {
        (Some(x), Some(y)) if y > 0.0 && x / y > 1.5 => x / y,
        _ => 1.0,
    };
    let g4 = first(TAG_COMPRESSION) == Some(4);
    let geometry = (
        first(TAG_WIDTH),
        first(TAG_HEIGHT),
        first(TAG_ROWS_PER_STRIP),
        first(TAG_PHOTOMETRIC),
    );
    let le = t.le;

    if reversed {
        for &(o, l) in &strips {
            let s = bytes
                .get_mut(o..o.saturating_add(l))
                .ok_or("Streifen außerhalb der Datei")?;
            s.iter_mut().for_each(|b| *b = b.reverse_bits());
        }
    }
    let img = if g4 {
        let (Some(w), Some(h), rows, photo) = geometry else {
            return Err("Fax-Seite ohne Breite/Höhe".into());
        };
        decode_g4(&bytes, &strips, w, h, rows.unwrap_or(h), photo == Some(1))?
    } else {
        bytes[4..8].copy_from_slice(&if le {
            ifd.to_le_bytes()
        } else {
            ifd.to_be_bytes()
        });
        let mut decoder = image::codecs::tiff::TiffDecoder::new(std::io::Cursor::new(bytes))
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(MAX_ALLOC);
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        image::DynamicImage::from_decoder(decoder)
            .map_err(|e| e.to_string())?
            .to_rgb8()
    };
    if stretch == 1.0 {
        return Ok(img);
    }
    let h = (img.height() as f32 * stretch).round() as u32;
    Ok(image::imageops::resize(
        &img,
        img.width(),
        h,
        image::imageops::FilterType::Triangle,
    ))
}

/// CCITT Group 4 (Fax), je Streifen. Viele Faxgeräte lassen weiße Zeilen am Ende weg; die
/// fehlenden Zeilen bleiben weiß. Bricht ein Streifen mitten ab, bleibt das Gelesene erhalten.
/// `black_is_zero`: Photometric 1 (selten); dann wie libtiff invertiert.
fn decode_g4(
    bytes: &[u8],
    strips: &[(usize, usize)],
    width: u32,
    height: u32,
    rows_per_strip: u32,
    black_is_zero: bool,
) -> Result<RgbImage, String> {
    let w16 = u16::try_from(width).map_err(|_| "Fax-Seite zu breit")?;
    let (white, black) = if black_is_zero {
        (0u8, 255u8)
    } else {
        (255, 0)
    };
    let mut img = image::GrayImage::from_pixel(width, height, image::Luma([white]));
    let mut row = 0u32;
    for (i, &(o, l)) in strips.iter().enumerate() {
        let data = bytes
            .get(o..o.saturating_add(l))
            .ok_or("Streifen außerhalb der Datei")?;
        let end = (i as u32 + 1).saturating_mul(rows_per_strip).min(height);
        let lines = u16::try_from(end.saturating_sub(row)).unwrap_or(u16::MAX);
        let mut y = row;
        // `None` bei kaputten Daten: die bis dahin gelesenen Zeilen genügen der OCR.
        let _ = fax::decoder::decode_g4(data.iter().copied(), w16, Some(lines), |t| {
            if y < height {
                for (x, c) in fax::decoder::pels(t, w16).enumerate() {
                    if c == fax::Color::Black {
                        img.put_pixel(x as u32, y, image::Luma([black]));
                    }
                }
            }
            y += 1;
        });
        row = end;
    }
    Ok(image::DynamicImage::ImageLuma8(img).to_rgb8())
}

/// Lesezugriff auf den TIFF-Kopf. Nur klassisches TIFF (kein BigTIFF).
struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(b: &'a [u8]) -> Option<Self> {
        let le = match b.get(..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let t = Self { b, le };
        (t.u16_at(2)? == 42).then_some(t)
    }

    fn u16_at(&self, o: usize) -> Option<u16> {
        let s: [u8; 2] = self.b.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(s)
        } else {
            u16::from_be_bytes(s)
        })
    }

    fn u32_at(&self, o: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    }

    /// Offsets der Bildverzeichnisse, eins je Seite.
    fn ifd_offsets(&self) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        let mut off = self.u32_at(4)?;
        // Obergrenze und Wiederholungsprüfung gegen kaputte Dateien mit IFD-Schleife.
        while off != 0 && out.len() < 1000 && !out.contains(&off) {
            out.push(off);
            let entries = self.u16_at(off as usize)? as usize;
            off = self.u32_at(off as usize + 2 + 12 * entries)?;
        }
        (!out.is_empty()).then_some(out)
    }

    /// Werte eines Tags vom Typ SHORT oder LONG; RATIONAL als [Zähler, Nenner].
    fn tag(&self, ifd: u32, tag: u16) -> Option<Vec<u32>> {
        let ifd = ifd as usize;
        let n = self.u16_at(ifd)? as usize;
        let entry = (0..n)
            .map(|i| ifd + 2 + 12 * i)
            .find(|&e| self.u16_at(e) == Some(tag))?;
        let typ = self.u16_at(entry + 2)?;
        let count = self.u32_at(entry + 4)? as usize;
        let (size, values) = match typ {
            3 => (2, count),
            4 => (4, count),
            5 => (4, count * 2),
            _ => return None,
        };
        let start = if size * values <= 4 {
            entry + 8
        } else {
            self.u32_at(entry + 8)? as usize
        };
        (0..values.min(100_000))
            .map(|i| match size {
                2 => self.u16_at(start + 2 * i).map(u32::from),
                _ => self.u32_at(start + 4 * i),
            })
            .collect()
    }

    fn rational(&self, ifd: u32, tag: u16) -> Option<f32> {
        match self.tag(ifd, tag)?.as_slice() {
            [num, den] if *den > 0 => Some(*num as f32 / *den as f32),
            _ => None,
        }
    }
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
        let t = Tiff::new(&bytes).unwrap();
        assert_eq!(t.ifd_offsets().map(|o| o.len()), Some(2));
        let p1 = decode_page(bytes.clone(), 0).unwrap();
        assert_eq!(p1.dimensions(), (4, 3));
        assert_eq!(p1.get_pixel(0, 0).0, [0, 0, 0]);
        let p2 = decode_page(bytes.clone(), 1).unwrap();
        assert_eq!(p2.dimensions(), (5, 2));
        assert_eq!(p2.get_pixel(4, 1).0, [255, 255, 255]);
        assert!(decode_page(bytes, 2).is_err());
    }

    /// Fax-Eigenheiten: FillOrder 2 (Bits je Byte umgekehrt) und 204×98 dpi. Von Hand gebaut,
    /// weil der Encoder keine 1-Bit-Bilder schreibt.
    #[test]
    fn fax_fill_order_and_half_height() {
        // 8×2 Pixel, BlackIsZero: erste Zeile nur das linke Pixel schwarz, zweite weiß.
        let rows = [0b0111_1111u8, 0xFF].map(u8::reverse_bits);
        let entries: [(u16, u16, u32, u32); 12] = [
            (256, 3, 1, 8),   // Breite
            (257, 3, 1, 2),   // Höhe
            (258, 3, 1, 1),   // Bits
            (259, 3, 1, 1),   // keine Kompression
            (262, 3, 1, 1),   // BlackIsZero
            (266, 3, 1, 2),   // FillOrder 2
            (273, 4, 1, 174), // StripOffsets
            (277, 3, 1, 1),   // SamplesPerPixel
            (278, 3, 1, 2),   // RowsPerStrip
            (279, 4, 1, 2),   // StripByteCounts
            (282, 5, 1, 158), // XResolution -> 204/1
            (283, 5, 1, 166), // YResolution -> 98/1
        ];
        let mut b = b"II*\0".to_vec();
        b.extend(8u32.to_le_bytes());
        b.extend((entries.len() as u16).to_le_bytes());
        for (tag, typ, count, value) in entries {
            b.extend(tag.to_le_bytes());
            b.extend(typ.to_le_bytes());
            b.extend(count.to_le_bytes());
            b.extend(value.to_le_bytes());
        }
        b.extend(0u32.to_le_bytes());
        assert_eq!(b.len(), 158);
        for v in [204u32, 1, 98, 1] {
            b.extend(v.to_le_bytes());
        }
        b.extend(rows);

        let p = decode_page(b, 0).unwrap();
        assert_eq!(p.dimensions(), (8, 4));
        assert_eq!(p.get_pixel(0, 0).0, [0, 0, 0]);
        assert_eq!(p.get_pixel(1, 0).0, [255, 255, 255]);
        assert_eq!(p.get_pixel(0, 3).0, [255, 255, 255]);
    }

    #[test]
    fn rejects_non_tiff() {
        assert!(Tiff::new(b"%PDF-1.4").is_none());
        assert!(Tiff::new(b"II*\0").and_then(|t| t.ifd_offsets()).is_none());
    }
}
