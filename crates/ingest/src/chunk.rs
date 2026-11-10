//! Seitenbewusster, absatzbasierter Chunker.
//!
//! - Chunks überspannen nie eine Seitengrenze (so ist die Fundstelle exakt).
//! - Absätze (Leerzeilen) werden bis ca. [`ChunkConfig::target`] Zeichen gesammelt,
//!   ein Chunk wird nie länger als [`ChunkConfig::max`] (außer ein einzelnes „Wort“ ist länger).
//! - Der nächste Chunk beginnt mit dem letzten Absatz des vorigen (Überlappung), sofern der
//!   höchstens `target / 2` lang ist.
//! - Zu lange Absätze werden an Zeilen, dann an Wortgrenzen geteilt; mitten in einem Wort wird nur
//!   geteilt, wenn ein einzelnes Wort länger als `max` ist (z. B. Base64-Blöcke).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    pub target: usize,
    pub max: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target: 2500,
            max: 3500,
        }
    }
}

/// Text einer Seite (`page` 1-basiert) bzw. eines ganzen Dokuments (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    pub page: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub page: Option<u32>,
    pub text: String,
}

/// Zerlegt alle Seiten in Chunks (Reihenfolge der Seiten bleibt erhalten).
pub fn chunk_pages(pages: &[PageText], cfg: &ChunkConfig) -> Vec<Chunk> {
    pages
        .iter()
        .flat_map(|p| {
            chunk_text(&p.text, cfg)
                .into_iter()
                .map(|text| Chunk { page: p.page, text })
        })
        .collect()
}

/// Zerlegt einen Text in Chunks.
pub fn chunk_text(text: &str, cfg: &ChunkConfig) -> Vec<String> {
    let units = split_units(text, cfg);
    let overlap_cap = cfg.target / 2;

    let mut chunks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut cur_len = 0usize;
    // Anzahl Einheiten in `cur`, die nicht nur Überlappung sind.
    let mut fresh = 0usize;

    for unit in &units {
        let ulen = unit.chars().count();
        let sep = if cur.is_empty() { 0 } else { 2 };
        if fresh > 0 && (cur_len >= cfg.target || cur_len + sep + ulen > cfg.max) {
            chunks.push(cur.join("\n\n"));
            let last = cur.last().copied();
            cur.clear();
            cur_len = 0;
            fresh = 0;
            if let Some(last) = last {
                let l = last.chars().count();
                if l <= overlap_cap && l + 2 + ulen <= cfg.max {
                    cur.push(last);
                    cur_len = l;
                }
            }
        }
        cur_len += if cur.is_empty() { 0 } else { 2 } + ulen;
        cur.push(unit);
        fresh += 1;
    }
    if fresh > 0 {
        chunks.push(cur.join("\n\n"));
    }
    chunks
}

/// Absätze (an Leerzeilen getrennt); Überlanges wird weiter zerlegt, sodass jede Einheit
/// höchstens `max` Zeichen hat.
fn split_units(text: &str, cfg: &ChunkConfig) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut units = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, units: &mut Vec<String>| {
        if !para.is_empty() {
            let joined = para.join("\n");
            para.clear();
            if joined.chars().count() <= cfg.max {
                units.push(joined);
            } else {
                units.extend(split_long(&joined, cfg));
            }
        }
    };
    for line in normalized.lines() {
        if line.trim().is_empty() {
            flush(&mut para, &mut units);
        } else {
            para.push(line.trim_end());
        }
    }
    flush(&mut para, &mut units);
    units
}

/// Teilt einen überlangen Absatz: erst nach Zeilen, dann nach Wörtern, zuletzt hart.
fn split_long(para: &str, cfg: &ChunkConfig) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    let push = |cur: &mut String, cur_len: &mut usize, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
            *cur_len = 0;
        }
    };

    for line in para.lines() {
        let llen = line.chars().count();
        if llen > cfg.max {
            push(&mut cur, &mut cur_len, &mut out);
            out.extend(split_words(line, cfg));
            continue;
        }
        let sep = usize::from(!cur.is_empty());
        if !cur.is_empty() && (cur_len >= cfg.target || cur_len + sep + llen > cfg.max) {
            push(&mut cur, &mut cur_len, &mut out);
        }
        if !cur.is_empty() {
            cur.push('\n');
            cur_len += 1;
        }
        cur.push_str(line);
        cur_len += llen;
    }
    push(&mut cur, &mut cur_len, &mut out);
    out
}

/// Teilt eine überlange Zeile an Wortgrenzen (Leerraum); Wörter > `max` werden hart geteilt.
fn split_words(line: &str, cfg: &ChunkConfig) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in line.split_whitespace() {
        let wlen = word.chars().count();
        if wlen > cfg.max {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            let chars: Vec<char> = word.chars().collect();
            out.extend(chars.chunks(cfg.max).map(|c| c.iter().collect::<String>()));
            continue;
        }
        let sep = usize::from(!cur.is_empty());
        if !cur.is_empty() && (cur_len >= cfg.target || cur_len + sep + wlen > cfg.max) {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if !cur.is_empty() {
            cur.push(' ');
            cur_len += 1;
        }
        cur.push_str(word);
        cur_len += wlen;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cfg() -> ChunkConfig {
        ChunkConfig::default()
    }

    fn para(n: usize, tag: &str) -> String {
        // ca. n Zeichen aus Wörtern mit Umlauten
        let mut s = String::new();
        let mut i = 0;
        while s.chars().count() < n {
            s.push_str(&format!("{tag}wörter{i} Übergröße "));
            i += 1;
        }
        s.trim_end().to_string()
    }

    #[test]
    fn empty_and_blank_yield_nothing() {
        assert!(chunk_text("", &cfg()).is_empty());
        assert!(chunk_text("  \n\n \r\n ", &cfg()).is_empty());
        assert!(chunk_pages(&[], &cfg()).is_empty());
    }

    #[test]
    fn short_text_is_one_chunk() {
        let c = chunk_text("Erster Absatz.\n\nZweiter Absatz.", &cfg());
        assert_eq!(c, ["Erster Absatz.\n\nZweiter Absatz."]);
    }

    #[test]
    fn crlf_is_normalized() {
        let c = chunk_text("Zeile eins\r\nZeile zwei\r\n\r\nNeuer Absatz", &cfg());
        assert_eq!(c, ["Zeile eins\nZeile zwei\n\nNeuer Absatz"]);
    }

    #[test]
    fn pages_never_merge_and_keep_page_numbers() {
        let pages = [
            PageText {
                page: Some(1),
                text: "Seite eins".into(),
            },
            PageText {
                page: Some(2),
                text: "   ".into(),
            },
            PageText {
                page: Some(3),
                text: "Seite drei".into(),
            },
        ];
        let c = chunk_pages(&pages, &cfg());
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].page, c[0].text.as_str()), (Some(1), "Seite eins"));
        assert_eq!((c[1].page, c[1].text.as_str()), (Some(3), "Seite drei"));
    }

    #[test]
    fn splits_at_paragraphs_near_target_with_overlap() {
        let paras: Vec<String> = (0..8).map(|i| para(900, &format!("p{i}x"))).collect();
        let text = paras.join("\n\n");
        let chunks = chunk_text(&text, &cfg());
        assert!(chunks.len() >= 3, "{}", chunks.len());
        for c in &chunks {
            assert!(c.chars().count() <= cfg().max, "{}", c.chars().count());
        }
        // Überlappung: der letzte Absatz jedes Chunks eröffnet den nächsten
        for pair in chunks.windows(2) {
            let last = pair[0].rsplit("\n\n").next().unwrap();
            assert!(pair[1].starts_with(last), "keine Überlappung");
        }
        // Alle Absätze kommen vor
        for p in &paras {
            assert!(chunks.iter().any(|c| c.contains(p.as_str())));
        }
    }

    #[test]
    fn huge_overlap_candidate_is_skipped() {
        // Absatz > target/2 wird nicht als Überlappung wiederholt
        let a = para(2000, "a");
        let b = para(2000, "b");
        let c = para(2000, "c");
        let chunks = chunk_text(&format!("{a}\n\n{b}\n\n{c}"), &cfg());
        let total: usize = chunks.iter().map(|c| c.matches("awörter0").count()).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn long_paragraph_splits_at_lines() {
        let lines: Vec<String> = (0..200)
            .map(|i| format!("Position {i:04} Pflaster verlegen"))
            .collect();
        let text = lines.join("\n"); // ein einziger Absatz ohne Leerzeile
        let chunks = chunk_text(&text, &cfg());
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.chars().count() <= cfg().max);
            // nur ganze Zeilen
            for l in c.lines() {
                assert!(lines.iter().any(|orig| orig == l), "Zeile zerrissen: {l}");
            }
        }
    }

    #[test]
    fn never_splits_inside_words() {
        let words: Vec<String> = (0..2000).map(|i| format!("Wörterbuch{i}ß")).collect();
        let text = words.join(" "); // eine einzige Riesenzeile
        let chunks = chunk_text(&text, &cfg());
        assert!(chunks.len() >= 3);
        let set: HashSet<&str> = words.iter().map(String::as_str).collect();
        for c in &chunks {
            assert!(c.chars().count() <= cfg().max);
            for w in c.split_whitespace() {
                assert!(set.contains(w), "Wort zerrissen: {w}");
            }
        }
    }

    #[test]
    fn giant_single_word_is_hard_split_on_char_boundaries() {
        let word = "ä".repeat(9000);
        let chunks = chunk_text(&word, &cfg());
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= cfg().max));
        assert_eq!(chunks.concat(), word);
    }

    #[test]
    fn small_config_works() {
        let small = ChunkConfig {
            target: 40,
            max: 60,
        };
        let text = "Alpha Beta Gamma\n\nDelta Epsilon Zeta\n\nEta Theta Iota\n\nKappa Lambda My";
        let chunks = chunk_text(text, &small);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|c| c.chars().count() <= 60));
    }

    #[test]
    fn overlap_only_chunk_is_never_emitted() {
        let small = ChunkConfig {
            target: 20,
            max: 30,
        };
        let chunks = chunk_text("aaaa bbbb cccc dddd eeee\n\nffff", &small);
        // letzter Chunk enthält neuen Inhalt
        assert!(chunks.last().unwrap().contains("ffff"));
        assert!(chunks.iter().all(|c| !c.is_empty()));
    }
}
