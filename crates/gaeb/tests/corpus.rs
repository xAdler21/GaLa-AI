//! Abgleich mit echten GAEB-Dateien aus dem Archiv. Die Dateien sind Kundendaten und liegen
//! deshalb nicht im Repo; ohne `GALA_GAEB_CORPUS` werden die Tests übersprungen.
//!
//! ```powershell
//! $env:GALA_GAEB_CORPUS = "<Ordner mit .d8x/.x8x-Dateien>"
//! cargo test -p gaeb --test corpus -- --nocapture
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gaeb::import;
use lv_model::{Lv, Oz, Position};

fn corpus() -> Option<PathBuf> {
    std::env::var_os("GALA_GAEB_CORPUS").map(PathBuf::from)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// `d83`, `x84` … in Kleinbuchstaben, sonst `None`.
fn gaeb_ext(p: &Path) -> Option<String> {
    let ext = p.extension()?.to_str()?.to_lowercase();
    (ext.len() == 3 && (ext.starts_with('d') || ext.starts_with('x')) && ext[1..].starts_with('8'))
        .then_some(ext)
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Für Langtexte: nur der Inhalt zählt. Leerraum und Platzhalter-Anführungszeichen setzen
/// die Programme unterschiedlich (`' > 3 cm'`, „fr ä sen“ in einer X86).
fn content(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '\'')
        .collect()
}

fn by_oz(lv: &Lv) -> HashMap<Oz, &Position> {
    lv.positions()
        .filter_map(|p| Some((p.oz.value.clone()?, p)))
        .collect()
}

#[test]
fn all_files_import() {
    let Some(dir) = corpus() else { return };
    let mut files = Vec::new();
    walk(&dir, &mut files);
    let mut failed = Vec::new();
    let (mut n, mut positions) = (0, 0);
    for f in files.iter().filter(|f| gaeb_ext(f).is_some()) {
        n += 1;
        match import(&std::fs::read(f).unwrap()) {
            Ok(imp) => {
                positions += imp.lv.positions().count();
                let without_oz = imp.lv.positions().filter(|p| p.oz.value.is_none()).count();
                if without_oz > 0 {
                    failed.push(format!("{}: {without_oz} Positionen ohne OZ", f.display()));
                }
            }
            Err(e) => failed.push(format!("{}: {e}", f.display())),
        }
    }
    println!(
        "{n} Dateien, {positions} Positionen, {} Probleme",
        failed.len()
    );
    for f in &failed {
        println!("  {f}");
    }
    assert!(failed.len() * 50 <= n, "zu viele Dateien nicht lesbar");
}

/// D-Datei gegen die XML-Fassung desselben LVs (gleicher Name, gleiche Phase).
#[test]
fn d90_matches_xml_twin() {
    let Some(dir) = corpus() else { return };
    let mut files = Vec::new();
    walk(&dir, &mut files);
    let mut stats: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut examples: Vec<String> = Vec::new();
    let mut pairs = 0;
    for d in files
        .iter()
        .filter(|f| gaeb_ext(f).is_some_and(|e| e.starts_with('d')))
    {
        let ext = gaeb_ext(d).unwrap();
        let Some(x) = files.iter().find(|x| {
            x.parent() == d.parent()
                && x.file_stem() == d.file_stem()
                && gaeb_ext(x).as_deref() == Some(&format!("x{}", &ext[1..]))
        }) else {
            continue;
        };
        pairs += 1;
        // In D84 stehen in 25/26 die Textergänzungen des Bieters, nicht die LV-Texte.
        let compare_texts = ext != "d84";
        let dl = import(&std::fs::read(d).unwrap()).unwrap().lv;
        let xl = import(&std::fs::read(x).unwrap()).unwrap().lv;
        let (dm, xm) = (by_oz(&dl), by_oz(&xl));
        let mut check = |what: &'static str, ok: bool, detail: String| {
            let s = stats.entry(what).or_default();
            s.0 += 1;
            if ok {
                s.1 += 1;
            } else if examples.iter().filter(|e| e.starts_with(what)).count() < 4 {
                examples.push(format!("{what}: {} {detail}", d.display()));
            }
        };
        check(
            "anzahl",
            dm.len() == xm.len(),
            format!("D {} / X {}", dm.len(), xm.len()),
        );
        for (oz, xp) in &xm {
            let Some(dp) = dm.get(oz) else {
                check("oz", false, format!("{oz} fehlt in D"));
                continue;
            };
            check("oz", true, String::new());
            check(
                "menge",
                dp.menge.value == xp.menge.value,
                format!("{oz}: {:?} / {:?}", dp.menge.value, xp.menge.value),
            );
            check(
                "einheit",
                dp.einheit.value == xp.einheit.value,
                format!("{oz}: {:?} / {:?}", dp.einheit.raw, xp.einheit.raw),
            );
            check(
                "ep",
                dp.ep.value == xp.ep.value,
                format!("{oz}: {:?} / {:?}", dp.ep.value, xp.ep.value),
            );
            check(
                "gp",
                dp.gp.value == xp.gp.value,
                format!("{oz}: {:?} / {:?}", dp.gp.value, xp.gp.value),
            );
            check(
                "art",
                dp.art == xp.art,
                format!("{oz}: {:?} / {:?}", dp.art, xp.art),
            );
            let (dk, xk) = (
                norm(dp.kurztext.value.as_deref().unwrap_or_default()),
                norm(xp.kurztext.value.as_deref().unwrap_or_default()),
            );
            if compare_texts && !xk.is_empty() {
                check("kurztext", dk == xk, format!("{oz}: {dk:?} / {xk:?}"));
            }
            let (dlt, xlt) = (
                content(dp.langtext.value.as_deref().unwrap_or_default()),
                content(xp.langtext.value.as_deref().unwrap_or_default()),
            );
            if compare_texts && !xlt.is_empty() {
                // Erste Abweichung mit etwas Umgebung zeigen.
                let (dc, xc): (Vec<char>, Vec<char>) =
                    (dlt.chars().collect(), xlt.chars().collect());
                let at = dc.iter().zip(&xc).take_while(|(a, b)| a == b).count();
                let around = |c: &[char]| {
                    c[at.saturating_sub(30)..(at + 50).min(c.len())]
                        .iter()
                        .collect::<String>()
                };
                check(
                    "langtext",
                    dlt == xlt,
                    format!("{oz} @{at}: {:?} / {:?}", around(&dc), around(&xc)),
                );
            }
        }
    }
    println!("{pairs} Paare");
    let mut keys: Vec<_> = stats.keys().copied().collect();
    keys.sort_unstable();
    for k in &keys {
        let (n, ok) = stats[k];
        println!(
            "  {k:9} {ok:5}/{n:5} = {:.1} %",
            ok as f64 * 100.0 / n as f64
        );
    }
    for e in &examples {
        println!("  ! {e}");
    }
    for k in ["oz", "menge", "einheit", "ep", "gp", "kurztext"] {
        if let Some(&(n, ok)) = stats.get(k) {
            assert!(ok * 100 >= n * 98, "{k}: nur {ok} von {n}");
        }
    }
}

/// Eigene Abgaben nachbauen: D83 + Preise aus der abgegebenen D84 → neue D84; die Preissätze
/// (`23`), Bereichssummen (`32`) und Netto in `99` müssen mit der echten Abgabe übereinstimmen.
#[test]
fn d84_matches_own_submissions() {
    let Some(dir) = corpus() else { return };
    let mut files = Vec::new();
    walk(&dir, &mut files);
    let records = |bytes: &[u8], kinds: &[&str]| -> Vec<String> {
        String::from_utf8_lossy(bytes)
            .split('\n')
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| l.len() >= 74 && kinds.contains(&&l[..2]))
            // OZ-Feld ohne Füllzeichen: `01  1` und `01001` meinen dieselbe Position.
            // Die 3. Nachkommastelle des EP (Sp. 23) steht mal als `0`, mal leer.
            .map(|l| {
                let rest = if &l[..2] == "23" && &l[22..23] == "0" {
                    format!("{} {}", &l[11..22], &l[23..74])
                } else {
                    l[11..74].to_string()
                };
                format!(
                    "{}{}{}",
                    &l[..2],
                    l[2..11].replace(' ', "0"),
                    rest.trim_end()
                )
            })
            .collect()
    };
    let (mut pairs, mut equal_23, mut equal_all) = (0, 0, 0);
    // Die D83 heißt oft anders als die Abgabe: gleiche Projektmappe (zwei Ebenen unter dem
    // Korpus) und alle bepreisten OZ kommen in ihr vor.
    let project = |p: &Path| {
        p.strip_prefix(&dir)
            .unwrap()
            .iter()
            .take(2)
            .collect::<PathBuf>()
    };
    let d83s: Vec<(&PathBuf, Lv)> = files
        .iter()
        .filter(|f| gaeb_ext(f).as_deref() == Some("d83"))
        .filter_map(|f| Some((f, import(&std::fs::read(f).ok()?).ok()?.lv)))
        .collect();
    for d84 in files
        .iter()
        .filter(|f| gaeb_ext(f).as_deref() == Some("d84"))
    {
        let given = std::fs::read(d84).unwrap();
        let Ok(priced) = import(&given) else { continue };
        let ozs: Vec<&Oz> = priced
            .lv
            .positions()
            .filter_map(|p| p.oz.value.as_ref())
            .collect();
        let Some((d83, _)) = d83s.iter().find(|(f, lv)| {
            project(f) == project(d84) && {
                let known = by_oz(lv);
                !ozs.is_empty() && ozs.iter().all(|oz| known.contains_key(*oz))
            }
        }) else {
            continue;
        };
        let orig = std::fs::read(d83).unwrap();
        // Wie in der App: LV aus der D83 (Positionsarten), Preise aus der Abgabe.
        let mut lv = import(&orig).unwrap().lv;
        for p in priced.lv.positions() {
            if let Some(target) = lv.position_mut(p.oz.value.as_ref().unwrap()) {
                target.ep = p.ep.clone();
                target.gp = p.gp.clone();
            }
        }
        pairs += 1;
        let out = gaeb::write_d84(&orig, &lv, None).unwrap();
        let (a, b) = (records(&out.bytes, &["23"]), records(&given, &["23"]));
        if a == b {
            equal_23 += 1;
        } else {
            let diff = a.iter().zip(&b).find(|(x, y)| x != y);
            println!(
                "  23 ≠ {}: {} / {} Sätze, z. B. {diff:?}",
                d84.display(),
                a.len(),
                b.len()
            );
        }
        // Ältere Abgaben haben 32 mit Nullen, neuere ohne Netto in 99 und mit abweichend
        // gerechneten Bereichssummen; verglichen wird deshalb nur, ob die Bereiche passen, und
        // die Beträge nur, wenn die Abgabe ein Netto in 99 trägt.
        let given_99 = records(&given, &["99"]);
        let has_net = given_99
            .first()
            .is_some_and(|l| l.len() > 3 && l[2..14.min(l.len())].trim_matches('0').trim() != "");
        let oz_only = |r: Vec<String>| {
            r.into_iter()
                .map(|l| l[..12.min(l.len())].to_string())
                .collect::<Vec<_>>()
        };
        let (ours_32, given_32) = (records(&out.bytes, &["32"]), records(&given, &["32"]));
        assert_eq!(
            oz_only(ours_32.clone()),
            oz_only(given_32.clone()),
            "Bereiche in {}",
            d84.display()
        );
        let filled_32 = given_32
            .iter()
            .any(|l| l[12..].trim_matches('0').trim() != "");
        let same_sums = !has_net
            || ((!filled_32 || ours_32 == given_32)
                && records(&out.bytes, &["99"])
                    .first()
                    .map(|l| l[..14].to_string())
                    == given_99.first().map(|l| l[..14].to_string()));
        if same_sums {
            equal_all += 1;
        } else {
            let (a, b) = (
                records(&out.bytes, &["32", "99"]),
                records(&given, &["32", "99"]),
            );
            let diff = a.iter().zip(&b).find(|(x, y)| x != y);
            println!(
                "  32/99 ≠ {}: {} / {}, z. B. {diff:?} / 99 {:?} {:?}",
                d84.display(),
                a.len(),
                b.len(),
                a.last(),
                b.last()
            );
        }
    }
    println!("{pairs} D83/D84-Paare: Preissätze gleich {equal_23}, Summen gleich {equal_all}");
    assert!(equal_23 * 10 >= pairs * 9, "Preissätze weichen zu oft ab");
    assert_eq!(equal_all, pairs, "Summen weichen ab");
}
