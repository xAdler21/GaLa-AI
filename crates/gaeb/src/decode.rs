use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};

use crate::GaebError;

/// Dekodiert eine GAEB-Datei: BOM vor XML-Deklaration, Standard UTF-8.
///
/// Ältere AVA-Programme schreiben oft ISO-8859-1, deklarieren aber UTF-8. Ist eine als UTF-8
/// deklarierte Datei kein gültiges UTF-8, wird daher Windows-1252 (Obermenge von ISO-8859-1)
/// angenommen statt Umlaute durch Ersatzzeichen zu ersetzen.
pub fn decode(bytes: &[u8]) -> Result<String, GaebError> {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        let (text, _) = encoding.decode_without_bom_handling(&bytes[bom_len..]);
        return Ok(text.into_owned());
    }
    let encoding = match declared_encoding(bytes) {
        Some(label) => Encoding::for_label(label.as_bytes())
            .ok_or_else(|| GaebError::Encoding(label.to_string()))?,
        None => UTF_8,
    };
    let (text, _, had_errors) = encoding.decode(bytes);
    if had_errors && encoding == UTF_8 {
        return Ok(WINDOWS_1252.decode(bytes).0.into_owned());
    }
    Ok(text.into_owned())
}

/// `encoding="…"` aus der XML-Deklaration (nur ASCII, nur am Dateianfang).
fn declared_encoding(bytes: &[u8]) -> Option<&str> {
    let head = &bytes[..bytes.len().min(256)];
    let decl_end = head.windows(2).position(|w| w == b"?>")?;
    let decl = std::str::from_utf8(&head[..decl_end]).ok()?;
    if !decl.trim_start().starts_with("<?xml") {
        return None;
    }
    let rest = &decl[decl.find("encoding")? + "encoding".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let value = &rest[1..];
    Some(&value[..value.find(quote)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATIN1_DECL: &str =
        r#"<?xml version="1.0" encoding="ISO-8859-1"?><a>Außenanlagen Grünfläche</a>"#;

    #[test]
    fn decodes_declared_latin1() {
        let (bytes, _, _) = WINDOWS_1252.encode(LATIN1_DECL);
        assert_eq!(decode(&bytes).unwrap(), LATIN1_DECL);
    }

    #[test]
    fn falls_back_when_utf8_is_declared_but_latin1_is_written() {
        let xml = LATIN1_DECL.replace("ISO-8859-1", "UTF-8");
        let (bytes, _, _) = WINDOWS_1252.encode(&xml);
        assert_eq!(decode(&bytes).unwrap(), xml);
    }

    #[test]
    fn honours_bom_and_plain_utf8() {
        let utf8 = "<a>Grün</a>";
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(utf8.as_bytes());
        assert_eq!(decode(&with_bom).unwrap(), utf8);
        assert_eq!(decode(utf8.as_bytes()).unwrap(), utf8);
    }

    #[test]
    fn rejects_unknown_encoding() {
        let xml = br#"<?xml version="1.0" encoding="x-unbekannt"?><a/>"#;
        assert!(matches!(decode(xml), Err(GaebError::Encoding(e)) if e == "x-unbekannt"));
    }
}
