//! GAEB 90 (D81–D86): Festformat mit 80 Zeichen je Zeile.
//!
//! Aufbau einer Zeile: Spalte 1–2 Satzart, 3–74 Inhalt, 75–80 laufende Nummer. Die Spalten
//! wurden an echten Dateien aus dem Archiv vermessen und gegen die XML-Fassung desselben LVs
//! geprüft (siehe `tests/corpus.rs`):
//!
//! | Satz | Inhalt |
//! |---|---|
//! | `00` | Kopf: Phase (Sp. 11–12), OZ-Maske (Sp. 63–71, z. B. `1122PPPPI`) |
//! | `01`/`02`/`03`/`04` | LV-Bezeichnung, Projekt, Auftraggeber, Bieter |
//! | `08` | Währung |
//! | `11`/`12`/`31` | Beginn, Bezeichnung und Ende eines LV-Bereichs (Titel) |
//! | `20` | Beginn einer Vorbemerkung, Text in folgenden `26` |
//! | `21` | Position: OZ (Sp. 3–11), Art (12–13, `E` = Bedarf ohne GB), Gruppe/Folge der Wahlposition (15–18), Menge (24–34, 3 Nachkommastellen), Einheit (35–38) |
//! | `23` | Preise: EP (Sp. 13–22, 2 Nachkommastellen, Sp. 23 optional 3.), GP (25–36, 2 Nachkommastellen) |
//! | `25`/`26` | Kurztext (Sp. 3–74) und Langtext (ab Sp. 6) |
//! | `32` | Summe eines LV-Bereichs (D84) |
//! | `41` | Leitbeschreibung (Text in folgenden `25`/`26`) |
//! | `99` | Ende |
//! | `T0`/`T1`/`T9` | Freitext vor dem Kopf |
//!
//! Ignoriert werden u. a. `27` (Mengenansatz/Kostengruppe), `28` (Verweis „wie Pos. …“, der
//! Text steht ohnehin in `25`), `30`, `79` (STLB-Bezug).
//!
//! Zeichensatz ist praktisch immer CP850 (DOS), selten Windows-1252.

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use lv_model::{
    Decimal, Field, Lv, LvSource, Node, Oz, OzMask, Position, PositionsArt, ReviewIssue, Titel,
    Unit, Vorbemerkung,
};

use crate::{GaebError, GaebImport, Phase};

/// CP850, Bytes 0x80–0xFF (aus der Python-Referenztabelle erzeugt).
const CP850_HIGH: &str = "\u{00C7}\u{00FC}\u{00E9}\u{00E2}\u{00E4}\u{00E0}\u{00E5}\u{00E7}\u{00EA}\u{00EB}\u{00E8}\u{00EF}\u{00EE}\u{00EC}\u{00C4}\u{00C5}\u{00C9}\u{00E6}\u{00C6}\u{00F4}\u{00F6}\u{00F2}\u{00FB}\u{00F9}\u{00FF}\u{00D6}\u{00DC}\u{00F8}\u{00A3}\u{00D8}\u{00D7}\u{0192}\u{00E1}\u{00ED}\u{00F3}\u{00FA}\u{00F1}\u{00D1}\u{00AA}\u{00BA}\u{00BF}\u{00AE}\u{00AC}\u{00BD}\u{00BC}\u{00A1}\u{00AB}\u{00BB}\u{2591}\u{2592}\u{2593}\u{2502}\u{2524}\u{00C1}\u{00C2}\u{00C0}\u{00A9}\u{2563}\u{2551}\u{2557}\u{255D}\u{00A2}\u{00A5}\u{2510}\u{2514}\u{2534}\u{252C}\u{251C}\u{2500}\u{253C}\u{00E3}\u{00C3}\u{255A}\u{2554}\u{2569}\u{2566}\u{2560}\u{2550}\u{256C}\u{00A4}\u{00F0}\u{00D0}\u{00CA}\u{00CB}\u{00C8}\u{0131}\u{00CD}\u{00CE}\u{00CF}\u{2518}\u{250C}\u{2588}\u{2584}\u{00A6}\u{00CC}\u{2580}\u{00D3}\u{00DF}\u{00D4}\u{00D2}\u{00F5}\u{00D5}\u{00B5}\u{00FE}\u{00DE}\u{00DA}\u{00DB}\u{00D9}\u{00FD}\u{00DD}\u{00AF}\u{00B4}\u{00AD}\u{00B1}\u{2017}\u{00BE}\u{00B6}\u{00A7}\u{00F7}\u{00B8}\u{00B0}\u{00A8}\u{00B7}\u{00B9}\u{00B3}\u{00B2}\u{25A0}\u{00A0}";

static CP850: LazyLock<Vec<char>> = LazyLock::new(|| CP850_HIGH.chars().collect());

/// Breite einer Zeile ohne Zeilenende.
const WIDTH: usize = 80;

/// Ist das eine GAEB-90-Datei? Erste Zeile ist ein Kopf- oder Freitextsatz in 80 Spalten.
pub(crate) fn looks_like_d90(bytes: &[u8]) -> bool {
    let first = bytes
        .split(|&b| b == b'\n')
        .next()
        .unwrap_or_default()
        .strip_suffix(b"\r")
        .unwrap_or_else(|| bytes.split(|&b| b == b'\n').next().unwrap_or_default());
    (first.starts_with(b"00") || first.starts_with(b"T0")) && first.len() == WIDTH
}

/// Dekodiert CP850, außer die Datei ist erkennbar Windows-1252 oder UTF-8.
pub(crate) fn decode_d90(bytes: &[u8]) -> String {
    if bytes.iter().any(|&b| b >= 0x80) && std::str::from_utf8(bytes).is_ok() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let count = |set: &[u8]| bytes.iter().filter(|b| set.contains(b)).count();
    // Umlaute und ²: in CP850 ä ö Ä Ö Ü ü ß ², in Windows-1252 ä ö Ä Ö Ü ß ² ³.
    let cp850 = count(&[0x84, 0x94, 0x8E, 0x99, 0x9A, 0x81, 0xE1, 0xFD]);
    let cp1252 = count(&[0xE4, 0xF6, 0xC4, 0xD6, 0xDC, 0xDF, 0xB2, 0xB3]);
    if cp1252 > cp850 {
        return encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned();
    }
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                CP850[(b - 0x80) as usize]
            }
        })
        .collect()
}

/// Kodiert für das Schreiben (D84). Zeichen ohne CP850-Entsprechung werden zu `?`.
pub(crate) fn encode_cp850(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| {
            if (c as u32) < 0x80 {
                c as u8
            } else {
                CP850
                    .iter()
                    .position(|&x| x == c)
                    .map_or(b'?', |i| 0x80 + i as u8)
            }
        })
        .collect()
}

/// Eine Zeile als Zeichen, auf 80 Spalten aufgefüllt.
pub(crate) struct Rec(Vec<char>);

impl Rec {
    pub(crate) fn new(line: &str) -> Self {
        let mut chars: Vec<char> = line.chars().collect();
        chars.resize(chars.len().max(WIDTH), ' ');
        Rec(chars)
    }

    pub(crate) fn kind(&self) -> String {
        self.cols(1, 2)
    }

    /// Spalten `from..=to`, 1-basiert wie in der Spezifikation.
    pub(crate) fn cols(&self, from: usize, to: usize) -> String {
        self.0[from - 1..to].iter().collect()
    }

    pub(crate) fn col(&self, n: usize) -> char {
        self.0[n - 1]
    }

    /// Letzte belegte Spalte im Inhaltsbereich (Sp. 3–74), 0 bei leerem Inhalt.
    fn last_used(&self) -> usize {
        (3..=74).rev().find(|&c| self.col(c) != ' ').unwrap_or(0)
    }

    /// Zeilen der Datei; leere Zeilen (Dateiende) entfallen.
    pub(crate) fn split(text: &str) -> Vec<Rec> {
        text.split('\n')
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.trim().is_empty())
            .map(Rec::new)
            .collect()
    }
}

/// Abschnitte der OZ-Maske, z. B. `1122PPPPI` → Ebene 1 (2), Ebene 2 (2), Position (4), Index (1).
#[derive(Debug, Clone)]
pub(crate) struct Mask {
    /// (Art, Startspalte im 9-stelligen Feld, Länge); Art `1`–`4`, `P` oder `I`.
    segments: Vec<(char, usize, usize)>,
}

impl Mask {
    pub(crate) fn parse(raw: &str) -> Option<Mask> {
        let mut segments: Vec<(char, usize, usize)> = Vec::new();
        for (i, c) in raw.chars().enumerate() {
            if !matches!(c, '1'..='4' | 'P' | 'I') {
                continue;
            }
            match segments.last_mut() {
                Some((k, start, len)) if *k == c && *start + *len == i => *len += 1,
                _ => segments.push((c, i, 1)),
            }
        }
        segments
            .iter()
            .any(|s| s.0 == 'P')
            .then_some(Mask { segments })
    }

    fn oz_mask(&self) -> OzMask {
        OzMask(
            self.segments
                .iter()
                .filter(|s| s.0 != 'I')
                .map(|s| s.2 as u8)
                .collect(),
        )
    }

    /// OZ aus dem 9-stelligen Feld. Titel haben nur Ebenen, Positionen zusätzlich `P`/`I`.
    /// Numerische Teile werden wie im XML-Reader auf die Stellenzahl aufgefüllt.
    pub(crate) fn oz(&self, field: &str) -> Option<Oz> {
        let chars: Vec<char> = field.chars().collect();
        let mut parts: Vec<String> = Vec::new();
        for &(kind, start, len) in &self.segments {
            let part: String = chars
                .get(start..start + len)
                .map(|c| c.iter().collect::<String>())
                .unwrap_or_default()
                .trim()
                .to_string();
            if part.is_empty() {
                continue;
            }
            if kind == 'I' {
                if part.chars().all(|c| c.is_ascii_alphabetic()) {
                    parts.last_mut()?.push_str(&part);
                } else {
                    parts.push(part);
                }
            } else if part.chars().all(|c| c.is_ascii_digit()) {
                parts.push(format!("{part:0>len$}"));
            } else {
                parts.push(part);
            }
        }
        Oz::parse(&parts.join("."))
    }
}

/// Ganzzahl aus Ziffern; Leerzeichen zählen als fehlend.
fn digits(raw: &str) -> Option<i64> {
    let t = raw.trim();
    (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
        .then(|| t.parse().ok())
        .flatten()
}

/// Text aus mehreren Zeilen. `hard_cut`: Das schreibende Programm bricht bei voller Breite
/// mitten im Wort um; dann wird eine volle Zeile ohne Trenner angehängt.
#[derive(Default)]
struct TextBuf {
    out: String,
    glue_next: bool,
}

impl TextBuf {
    fn push(&mut self, text: &str, full: bool, sep: char) {
        if !self.out.is_empty() && !self.glue_next {
            self.out.push(sep);
        }
        self.out.push_str(text.trim_end());
        self.glue_next = full;
    }

    fn take(&mut self) -> String {
        self.glue_next = false;
        std::mem::take(&mut self.out).trim().to_string()
    }
}

enum Pending {
    None,
    Position {
        pos: Box<Position>,
        kurz: TextBuf,
        lang: TextBuf,
    },
    Text(TextBuf),
}

struct Builder {
    mask: Mask,
    root: Vec<Node>,
    stack: Vec<(usize, Titel)>,
    pending: Pending,
    /// Wahlpositionen: Gruppe → OZ der Grundposition.
    groups: HashMap<String, Oz>,
    /// Langtext-Zeilen, die bis Sp. 60 reichen, gelten als mitten im Wort umbrochen.
    hard_cut: bool,
    /// Folgende `12`-Sätze gehören noch zur Bezeichnung des zuletzt geöffneten Bereichs.
    title_label_open: bool,
}

impl Builder {
    fn push_node(&mut self, node: Node) {
        match self.stack.last_mut() {
            Some((_, t)) => t.children.push(node),
            None => self.root.push(node),
        }
    }

    fn flush(&mut self) {
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => {}
            Pending::Position {
                mut pos,
                mut kurz,
                mut lang,
            } => {
                let kurz = kurz.take();
                let lang = lang.take();
                let kurztext = if kurz.is_empty() {
                    lang.lines().next().unwrap_or_default().to_string()
                } else {
                    kurz
                };
                pos.kurztext = Field::certain(kurztext);
                if !lang.is_empty() {
                    pos.langtext = Field::certain(lang);
                }
                self.push_node(Node::Position(pos));
            }
            Pending::Text(mut buf) => {
                let text = buf.take();
                if !text.is_empty() {
                    self.push_node(Node::Vorbemerkung(Vorbemerkung {
                        text: Field::certain(text),
                    }));
                }
            }
        }
    }

    fn close_title(&mut self) {
        if let Some((_, t)) = self.stack.pop() {
            self.push_node(Node::Titel(t));
        }
    }

    fn open_title(&mut self, field: &str) {
        let oz = self.mask.oz(field);
        let depth = oz.as_ref().map_or(1, Oz::depth);
        while self.stack.last().is_some_and(|(d, _)| *d >= depth) {
            self.close_title();
        }
        self.stack.push((
            depth,
            Titel {
                oz: Field::parsed(oz, field.trim()),
                bezeichnung: Field::certain(String::new()),
                children: Vec::new(),
            },
        ));
        self.title_label_open = true;
    }

    fn end_title(&mut self, field: &str) {
        let depth = self.mask.oz(field).as_ref().map_or(1, Oz::depth);
        while self.stack.last().is_some_and(|(d, _)| *d >= depth) {
            self.close_title();
        }
    }

    fn title_label(&mut self, text: &str) {
        if let Some((_, t)) = self.stack.last_mut() {
            let label = t.bezeichnung.value.get_or_insert_with(String::new);
            let text = text.trim();
            if !text.is_empty() {
                if !label.is_empty() {
                    label.push(' ');
                }
                label.push_str(text);
            }
        }
    }

    fn position(&mut self, r: &Rec) {
        let field = r.cols(3, 11);
        let oz = self.mask.oz(&field);
        let menge_raw = r.cols(24, 34);
        let menge = digits(&menge_raw).map(|n| Decimal::new(n, 3));
        let einheit_raw = r.cols(35, 38).trim().to_string();
        let group = r.cols(15, 17).trim().to_string();
        let (p1, p2) = (r.col(12), r.col(13));
        if p1 == 'G'
            && !group.is_empty()
            && let Some(oz) = &oz
        {
            self.groups.insert(group.clone(), oz.clone());
        }
        let (art, grund_oz) = if p1 == 'A' {
            (PositionsArt::Alternativ, self.groups.get(&group).cloned())
        } else if p1 == 'S' {
            (PositionsArt::Stundenlohn, None)
        } else if p2 == 'E' {
            // `M` (Bedarf mit Gesamtbetrag) zählt in die Summe, wie `WithTotal` im XML.
            (PositionsArt::Bedarf, None)
        } else {
            (PositionsArt::Normal, None)
        };
        let einheit = if einheit_raw.is_empty() {
            Field::absent()
        } else {
            let mut f = Field::parsed(Unit::parse(&einheit_raw), einheit_raw.clone());
            if f.value.is_none() {
                f.issues = vec![ReviewIssue::UnknownUnit];
            }
            f
        };
        let pos = Position {
            oz: Field::parsed(oz, field.trim()),
            kurztext: Field::absent(),
            langtext: Field::absent(),
            menge: if menge_raw.trim().is_empty() {
                Field::absent()
            } else {
                Field::parsed(menge, menge_raw.trim())
            },
            einheit,
            ep: Field::absent(),
            gp: Field::absent(),
            art,
            grund_oz,
        };
        self.pending = Pending::Position {
            pos: Box::new(pos),
            kurz: TextBuf::default(),
            lang: TextBuf::default(),
        };
    }

    /// Preissatz `23`: gehört zur offenen Position mit gleicher OZ, sonst (D84 ohne Texte)
    /// entsteht eine Position nur mit OZ und Preisen.
    fn prices(&mut self, r: &Rec) {
        let field = r.cols(3, 11);
        let ep = digits(&r.cols(13, 22)).map(|n| {
            let third = r.col(23).to_digit(10).unwrap_or(0);
            Decimal::new(n * 10 + i64::from(third), 3)
        });
        let gp = digits(&r.cols(25, 36)).map(|n| Decimal::new(n, 2));
        let matches = matches!(&self.pending, Pending::Position { pos, .. }
            if pos.oz.raw.as_deref() == Some(field.trim()));
        if !matches {
            self.flush();
            let oz = self.mask.oz(&field);
            let mut pos = Position::new(oz.clone().unwrap_or_else(|| Oz::parse("0").unwrap()), "");
            pos.oz = Field::parsed(oz, field.trim());
            pos.kurztext = Field::absent();
            self.pending = Pending::Position {
                pos: Box::new(pos),
                kurz: TextBuf::default(),
                lang: TextBuf::default(),
            };
        }
        if let Pending::Position { pos, .. } = &mut self.pending {
            if let Some(ep) = ep {
                pos.ep = Field::parsed(Some(ep), r.cols(13, 23).trim());
            }
            if let Some(gp) = gp {
                pos.gp = Field::parsed(Some(gp), r.cols(25, 36).trim());
            }
        }
    }

    fn short_text(&mut self, r: &Rec) {
        let full = r.last_used() >= 72;
        let text = text_after_marker(r, 3);
        match &mut self.pending {
            Pending::Position { kurz, .. } => kurz.push(&text, full, ' '),
            Pending::Text(buf) => buf.push(&text, full, '\n'),
            Pending::None => {}
        }
    }

    fn long_text(&mut self, r: &Rec) {
        let text = text_after_marker(r, 6);
        let full = self.hard_cut && r.last_used() == 60;
        match &mut self.pending {
            Pending::Position { lang, .. } => lang.push(&text, full, '\n'),
            Pending::Text(buf) => buf.push(&text, full, '\n'),
            Pending::None => {
                // Langtext direkt nach einem Titel: als Vorbemerkung des Titels führen.
                let mut buf = TextBuf::default();
                buf.push(&text, full, '\n');
                self.pending = Pending::Text(buf);
            }
        }
    }
}

/// Text einer `25`/`26`-Zeile. Sp. 3–5 können eine Textergänzung kennzeichnen (`01B` =
/// Bieter füllt aus, `01A` = Auftraggeber); dann beginnt der Text in Sp. 6. Die Anführungszeichen
/// um den Platzhalter bleiben stehen (zeigen, was auszufüllen ist). `start`: Textbeginn ohne Kennzeichen.
fn text_after_marker(r: &Rec, start: usize) -> String {
    let marker: Vec<char> = r.cols(3, 5).chars().collect();
    let is_marker =
        marker[0].is_ascii_digit() && marker[1].is_ascii_digit() && matches!(marker[2], 'A' | 'B');
    if is_marker {
        let text = r.cols(6, 74);
        return text.trim_end().to_string();
    }
    r.cols(start, 74)
}

/// Liest eine GAEB-90-Datei.
pub(crate) fn read_d90(bytes: &[u8]) -> Result<GaebImport, GaebError> {
    let text = decode_d90(bytes);
    let recs = Rec::split(&text);
    let header = recs
        .iter()
        .find(|r| r.kind() == "00")
        .ok_or(GaebError::Missing("Satz 00 (Kopf)"))?;
    let phase =
        Phase::from_dp(&header.cols(11, 12)).ok_or(GaebError::Missing("Phase in Satz 00"))?;
    let mask =
        Mask::parse(&header.cols(63, 71)).ok_or(GaebError::Missing("OZ-Maske in Satz 00"))?;
    let mut warnings = Vec::new();
    if recs
        .iter()
        .any(|r| r.kind() == "77" && r.cols(3, 7) == "MASKE")
    {
        warnings.push(
            "Erweiterte OZ (Satz 77) wird nicht unterstützt; OZ nach der Maske im Kopfsatz".into(),
        );
    }

    let lang: Vec<&Rec> = recs.iter().filter(|r| r.kind() == "26").collect();
    let at_60 = lang.iter().filter(|r| r.last_used() == 60).count();
    let hard_cut = !lang.is_empty() && at_60 * 4 > lang.len();

    let mut b = Builder {
        mask: mask.clone(),
        root: Vec::new(),
        stack: Vec::new(),
        pending: Pending::None,
        groups: HashMap::new(),
        hard_cut,
        title_label_open: false,
    };
    let mut name_lv = String::new();
    let mut name_prj = String::new();
    let mut prj_seen = false;
    let mut currency = String::new();
    let mut unknown = BTreeSet::new();

    for r in &recs {
        let kind = r.kind();
        if kind != "12" {
            b.title_label_open = false;
        }
        match kind.as_str() {
            "00" | "03" | "04" | "06" | "27" | "28" | "30" | "32" | "42" | "77" | "79" | "99"
            | "T9" => {}
            "01" => name_lv = r.cols(3, 42).trim().trim_end_matches(" -").to_string(),
            "02" => {
                // Nur die erste Zeile zählt; ist sie ein Programmvermerk, enthält der ganze
                // 02-Block Exportangaben statt der Projektbezeichnung.
                if !prj_seen {
                    prj_seen = true;
                    let line = r.cols(3, 74).trim().to_string();
                    if !crate::is_software_stamp(&line) {
                        name_prj = line;
                    }
                }
            }
            "08" => currency = r.cols(3, 8).trim().to_string(),
            "T0" => {
                b.flush();
                b.pending = Pending::Text(TextBuf::default());
            }
            "T1" => {
                if let Pending::Text(buf) = &mut b.pending {
                    buf.push(&r.cols(3, 74), false, '\n');
                }
            }
            "11" => {
                b.flush();
                b.open_title(&r.cols(3, 11));
            }
            "12" => {
                if b.title_label_open {
                    b.title_label(&r.cols(3, 74));
                    b.title_label_open = true;
                }
            }
            "31" => {
                b.flush();
                b.end_title(&r.cols(3, 11));
            }
            "20" => {
                b.flush();
                b.pending = Pending::Text(TextBuf::default());
            }
            "41" => {
                b.flush();
                let mut buf = TextBuf::default();
                buf.push(&r.cols(7, 74), false, '\n');
                b.pending = Pending::Text(buf);
            }
            "21" => {
                b.flush();
                b.position(r);
            }
            "23" => b.prices(r),
            "25" => b.short_text(r),
            "26" => b.long_text(r),
            other => {
                unknown.insert(other.to_string());
            }
        }
    }
    b.flush();
    while !b.stack.is_empty() {
        b.close_title();
    }
    for kind in unknown {
        warnings.push(format!("Satzart {kind} wird ignoriert"));
    }

    let name = [name_prj, name_lv]
        .into_iter()
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| "GAEB-LV".into());
    let currency = match currency.to_uppercase().as_str() {
        "" | "EURO" | "EUR" => "EUR".to_string(),
        other => other.to_string(),
    };
    Ok(GaebImport {
        phase,
        version: Some("90".into()),
        lv: Lv {
            name,
            source: LvSource::Gaeb {
                format: format!("D{}", phase.dp()),
            },
            currency,
            oz_mask: Some(mask.oz_mask()),
            nodes: b.root,
        },
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp850_roundtrip_and_detection() {
        assert_eq!(CP850.len(), 128);
        assert_eq!(decode_d90(&[0x84, 0x94, 0x81, 0xE1, 0xFD, 0xFC]), "äöüß²³");
        assert_eq!(encode_cp850("Maßnahme m³ ä"), b"Ma\xE1nahme m\xFC \x84");
        // Windows-1252 überwiegt: ä ö ü.
        assert_eq!(decode_d90(b"Gr\xFCn \xE4 \xF6 \xDF"), "Grün ä ö ß");
    }

    #[test]
    fn mask_and_oz() {
        let m = Mask::parse("1122PPPPI").unwrap();
        assert_eq!(m.oz_mask(), OzMask(vec![2, 2, 4]));
        assert_eq!(m.oz("01010001 ").unwrap().to_string(), "01.01.0001");
        assert_eq!(m.oz("0101     ").unwrap().to_string(), "01.01");
        assert_eq!(m.oz(" 1 2   3A").unwrap().to_string(), "01.02.0003A");
        let m = Mask::parse("11122PPPP").unwrap();
        assert_eq!(m.oz("01500   1").unwrap().to_string(), "015.00.0001");
    }

    fn d90(lines: &[&str]) -> Vec<u8> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{l:<74}{:06}\r\n", i + 1))
            .collect::<String>()
            .into_bytes()
    }

    #[test]
    fn dataflor_export_note_is_not_the_name() {
        let head = "00        84                                                  1122PPI0090";
        let lv = "01Landschaftsbauarbeiten -                29.02.24                     X";
        let file = d90(&[
            head,
            lv,
            "02 GAEB version used: 320",
            "02DATAflor GAEB-Import",
            "02 Benutzer: Bahn, David Datum: 26.02.2024 11:52:06 Dateiname",
            "99",
        ]);
        assert_eq!(read_d90(&file).unwrap().lv.name, "Landschaftsbauarbeiten");
        let file = d90(&[head, lv, "02Kita Sonnenschein", "99"]);
        assert_eq!(read_d90(&file).unwrap().lv.name, "Kita Sonnenschein");
    }
}
