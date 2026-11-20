//! Preis-Historie über echte GAEB-Dateien (Kundendaten, nicht im Repo). Läuft nur mit
//! `GALA_GAEB_CORPUS` und zeigt, wie viele Preise aus Abgaben übernommen würden.

use ingest::extract::extract_gaeb_with_prices;

#[test]
fn offers_in_corpus_yield_prices() {
    let Some(dir) = std::env::var_os("GALA_GAEB_CORPUS") else {
        return;
    };
    let (mut offers, mut with_prices, mut rows, mut without_text) = (0, 0, 0, Vec::new());
    for e in walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = e.path();
        let ext = p
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !(ext.ends_with("84") || ext.ends_with("86")) || ext.len() != 3 {
            continue;
        }
        offers += 1;
        match extract_gaeb_with_prices(p) {
            Ok((_, r)) if !r.is_empty() => {
                with_prices += 1;
                rows += r.len();
            }
            Ok(_) => without_text.push(p.display().to_string()),
            Err(e) => without_text.push(format!("{}: {e}", p.display())),
        }
    }
    println!("{offers} Abgaben/Aufträge, {with_prices} mit Preisen, {rows} Preiszeilen");
    for f in &without_text {
        println!("  ohne Preise: {f}");
    }
    assert!(
        with_prices * 2 >= offers,
        "weniger als die Hälfte liefert Preise"
    );
}
