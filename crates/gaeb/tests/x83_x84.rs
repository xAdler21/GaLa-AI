//! Import einer realistischen X83 und Roundtrip zur Angebotsabgabe X84.

use gaeb::{GaebError, Phase, read_xml, write_x84};
use lv_model::{Decimal, Field, Lv, Node, Oz, OzMask, Position, PositionsArt, Unit};
use rust_decimal_macros::dec;

const X83: &str = include_str!("fixtures/kita.x83");

fn oz(s: &str) -> Oz {
    Oz::parse(s).unwrap()
}

fn position<'a>(lv: &'a Lv, o: &str) -> &'a Position {
    lv.positions()
        .find(|p| p.oz.value.as_ref() == Some(&oz(o)))
        .unwrap_or_else(|| panic!("Position {o} fehlt"))
}

fn set_ep(lv: &mut Lv, o: &str, ep: Decimal) {
    lv.position_mut(&oz(o)).unwrap().ep = Field::certain(ep);
}

fn text(field: &Field<String>) -> &str {
    field.value.as_deref().unwrap_or_default()
}

#[test]
fn reads_project_and_structure() {
    let import = read_xml(X83).unwrap();
    assert_eq!(import.phase, Phase::X83);
    assert_eq!(import.version.as_deref(), Some("3.3"));

    let lv = &import.lv;
    assert_eq!(lv.name, "Außenanlagen Kita Sonnenschein");
    assert_eq!(lv.currency, "EUR");
    assert_eq!(lv.oz_mask, Some(OzMask(vec![2, 2, 4])));

    let ozs: Vec<String> = lv
        .positions()
        .map(|p| p.oz.value.as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        ozs,
        [
            "01.01.0010",
            "01.01.0020",
            "01.01.0030",
            "01.01.0040",
            "02.01.0010",
            "02.01.0020"
        ]
    );

    let Node::Titel(pflaster) = &lv.nodes[0] else {
        panic!("Titel erwartet")
    };
    assert_eq!(pflaster.oz.value, Some(oz("01")));
    assert_eq!(text(&pflaster.bezeichnung), "Pflasterarbeiten");
    let Node::Titel(wege) = &pflaster.children[0] else {
        panic!("Untertitel erwartet")
    };
    assert_eq!(wege.oz.value, Some(oz("01.01")));
    assert_eq!(text(&wege.bezeichnung), "Wege & Plätze");
    let Node::Vorbemerkung(remark) = &wege.children[0] else {
        panic!("Vorbemerkung erwartet")
    };
    assert_eq!(
        text(&remark.text),
        "Alle Pflasterflächen mit 2,5 % Gefälle herstellen."
    );

    assert!(
        import.warnings.iter().any(|w| w.contains("02.01.0090")),
        "{:?}",
        import.warnings
    );
}

#[test]
fn reads_position_details() {
    let lv = read_xml(X83).unwrap().lv;

    let pflaster = position(&lv, "01.01.0010");
    assert_eq!(pflaster.menge.value, Some(dec!(120)));
    assert_eq!(pflaster.einheit.value, Some(Unit::M2));
    assert_eq!(
        text(&pflaster.kurztext),
        "Betonsteinpflaster 20x10x8 liefern und verlegen"
    );
    assert_eq!(
        text(&pflaster.langtext),
        "Betonsteinpflaster 20x10x8 cm, grau,\nim Sandbett verlegen.\nFugen mit Brechsand verfüllen."
    );
    assert_eq!(pflaster.art, PositionsArt::Normal);
    assert_eq!(pflaster.ep.value, None);

    let bord = position(&lv, "01.01.0020");
    assert_eq!(bord.art, PositionsArt::Bedarf);
    assert_eq!(bord.einheit.value, Some(Unit::M));

    assert_eq!(position(&lv, "01.01.0030").art, PositionsArt::Normal);
    let alternative = position(&lv, "01.01.0040");
    assert_eq!(alternative.art, PositionsArt::Alternativ);
    assert_eq!(alternative.grund_oz, Some(oz("01.01.0030")));

    let stunde = position(&lv, "02.01.0010");
    assert_eq!(stunde.art, PositionsArt::Stundenlohn);
    assert_eq!(stunde.einheit.value, Some(Unit::H));

    let be = position(&lv, "02.01.0020");
    assert_eq!(
        text(&be.kurztext),
        "Baustelleneinrichtung vorhalten und räumen"
    );
    assert_eq!(
        text(&be.langtext),
        "Baustelleneinrichtung vorhalten und räumen\ninkl. Bauzaun."
    );
    assert_eq!(be.einheit.value, Some(Unit::Psch));

    assert!(lv.positions().all(|p| !p.needs_review()));
}

#[test]
fn x84_roundtrip_inserts_prices() {
    let mut lv = read_xml(X83).unwrap().lv;
    set_ep(&mut lv, "01.01.0010", dec!(45.50));
    set_ep(&mut lv, "01.01.0020", dec!(38.20));
    set_ep(&mut lv, "02.01.0010", dec!(58));
    set_ep(&mut lv, "02.01.0020", dec!(1500));

    let export = write_x84(X83, &lv).unwrap();
    assert_eq!(export.missing_prices, [oz("01.01.0030"), oz("01.01.0040")]);
    assert!(
        export
            .xml
            .contains("http://www.gaeb.de/GAEB_DA_XML/DA84/3.3")
    );
    assert!(
        !export.xml.contains("DA83"),
        "Namespace/schemaLocation nicht umgestellt"
    );
    assert!(export.xml.contains("<DP>84</DP>"));
    assert!(
        export
            .xml
            .contains("<QU>m2</QU><UP>45.50</UP><IT>5460.00</IT>")
    );
    // IDs und Texte bleiben unverändert
    assert!(export.xml.contains(r#"<Item ID="I1" RNoPart="10">"#));
    assert!(export.xml.contains("Wege &amp; Plätze"));

    let back = read_xml(&export.xml).unwrap();
    assert_eq!(back.phase, Phase::X84);
    let pflaster = position(&back.lv, "01.01.0010");
    assert_eq!(pflaster.ep.value, Some(dec!(45.50)));
    assert_eq!(pflaster.gp.value, Some(dec!(5460.00)));
    assert_eq!(
        position(&back.lv, "02.01.0020").gp.value,
        Some(dec!(1500.00))
    );
    // Bedarfsposition zählt nicht zur Summe: 5460 + 10 × 58 + 1500
    assert_eq!(back.lv.total(), dec!(7540.00));

    for (a, b) in lv.positions().zip(back.lv.positions()) {
        assert_eq!(a.oz, b.oz);
        assert_eq!(a.kurztext, b.kurztext);
        assert_eq!(a.langtext, b.langtext);
        assert_eq!(a.menge, b.menge);
        assert_eq!(a.art, b.art);
    }
}

#[test]
fn reexport_replaces_existing_prices() {
    let mut lv = read_xml(X83).unwrap().lv;
    set_ep(&mut lv, "01.01.0010", dec!(45.50));
    let first = write_x84(X83, &lv).unwrap();

    set_ep(&mut lv, "01.01.0010", dec!(47));
    let second = write_x84(&first.xml, &lv).unwrap();
    assert_eq!(second.xml.matches("<UP>").count(), 1);
    assert!(second.xml.contains("<UP>47.00</UP><IT>5640.00</IT>"));
    assert!(!second.xml.contains("45.50"));
    assert!(second.xml.contains("<DP>84</DP>"));
}

#[test]
fn imports_latin1_and_exports_utf8() {
    let latin = X83.replacen(r#"encoding="UTF-8""#, r#"encoding="ISO-8859-1""#, 1);
    let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(&latin);
    let import = gaeb::import(&bytes).unwrap();
    assert_eq!(import.lv.name, "Außenanlagen Kita Sonnenschein");

    let xml = gaeb::decode(&bytes).unwrap();
    let export = write_x84(&xml, &import.lv).unwrap();
    assert!(
        export
            .xml
            .starts_with(r#"<?xml version="1.0" encoding="UTF-8"?>"#)
    );
}

#[test]
fn rejects_invalid_input() {
    assert!(matches!(read_xml("<foo/>"), Err(GaebError::NotGaeb(name)) if name == "foo"));
    assert!(matches!(read_xml("<GAEB>"), Err(GaebError::Parse(_))));
    assert!(matches!(
        read_xml("<GAEB><Award/></GAEB>"),
        Err(GaebError::Missing("Award/DP"))
    ));
}
