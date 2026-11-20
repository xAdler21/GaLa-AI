//! Kurzfassung von GAEB-Dateien als JSON (OZ und Kurztexte je Datei), für Auswertungen am
//! Korpus: `cargo run -p gaeb --example gaeb_summary -- <Ordner> > summary.json`

use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("Ordner angeben");
    let root = Path::new(&root);
    let mut files = Vec::new();
    walk(root, &mut files);
    let mut list = Vec::new();
    for f in files {
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !(ext == "x83" || ext == "d83" || ext == "x81" || ext == "d81") {
            continue;
        }
        let Ok(imp) = std::fs::read(&f)
            .map_err(drop)
            .and_then(|b| gaeb::import(&b).map_err(drop))
        else {
            continue;
        };
        let (oz, kurz): (Vec<String>, Vec<String>) = imp
            .lv
            .positions()
            .filter_map(|p| {
                Some((
                    p.oz.value.as_ref()?.to_string(),
                    p.kurztext.value.clone().unwrap_or_default(),
                ))
            })
            .unzip();
        list.push(serde_json::json!({
            "rel": f.strip_prefix(root).unwrap().to_string_lossy(),
            "oz": oz,
            "kurz": kurz,
        }));
    }
    println!("{}", serde_json::Value::Array(list));
}
