use std::borrow::Cow;
use std::collections::HashMap;

use lv_model::{Decimal, Lv, Oz};
use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::name::QName;

use crate::{GaebError, Phase, read};

/// Ergebnis des X84-Exports.
#[derive(Debug, Clone)]
pub struct X84Export {
    pub xml: String,
    /// Positionen ohne Einheitspreis; sie stehen ohne Preis im X84.
    pub missing_prices: Vec<Oz>,
}

/// Erzeugt die Angebotsabgabe X84 aus der Original-Ausschreibung (meist X83) und den Preisen
/// des bepreisten LVs. Zuordnung über die OZ.
///
/// Die Datei wird als Ereignisstrom durchgereicht und nur minimal verändert:
/// - `DP` wird 84, Namespace und `schemaLocation` zeigen auf DA84,
/// - jedes bepreiste `Item` bekommt `UP`/`IT` direkt hinter `QU` (sonst vor `Description`),
/// - vorhandene Preisangaben (Re-Export eines X84) werden ersetzt.
pub fn write_x84(original_xml: &str, lv: &Lv) -> Result<X84Export, GaebError> {
    let (import, item_ozs) = read::read(original_xml)?;
    let source_dp = import.phase.dp();

    let prices: HashMap<&Oz, (Decimal, Decimal)> = lv
        .positions()
        .filter_map(|p| {
            let oz = p.oz.value.as_ref()?;
            let ep = p.ep.value?;
            // Ohne Menge (z. B. Pauschale ohne Qty) entspricht der Gesamtpreis dem EP.
            let gp = p.gp.value.or_else(|| p.computed_gp()).unwrap_or(ep);
            Some((oz, (ep, gp)))
        })
        .collect();
    let item_prices: Vec<Option<(Decimal, Decimal)>> = item_ozs
        .iter()
        .map(|oz| oz.as_ref().and_then(|oz| prices.get(oz).copied()))
        .collect();
    let missing_prices = item_ozs
        .iter()
        .flatten()
        .filter(|oz| !prices.contains_key(oz))
        .cloned()
        .collect();

    let xml = transform(original_xml, source_dp, &item_prices)?;
    Ok(X84Export {
        xml,
        missing_prices,
    })
}

struct ItemState {
    depth: usize,
    price: Option<(Decimal, Decimal)>,
    /// Namespace-Präfix des Item-Elements (meist leer), damit eingefügte Elemente passen.
    prefix: String,
    inserted: bool,
}

fn transform(
    xml: &str,
    source_dp: u8,
    item_prices: &[Option<(Decimal, Decimal)>],
) -> Result<String, GaebError> {
    let stream_err = |e: quick_xml::Error| GaebError::Write(e.to_string());
    let io_err = |e: std::io::Error| GaebError::Write(e.to_string());

    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::with_capacity(xml.len() + 4096));
    let mut depth = 0usize;
    let mut item_index = 0usize;
    let mut item: Option<ItemState> = None;
    let mut skip_from: Option<usize> = None;
    let mut in_dp = false;

    loop {
        let event = reader.read_event().map_err(stream_err)?;
        match event {
            Event::Eof => break,
            Event::Decl(_) => {
                // Ausgabe ist immer UTF-8, unabhängig von der Kodierung der Vorlage.
                writer
                    .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
                    .map_err(io_err)?;
            }
            Event::Start(start) => {
                depth += 1;
                if skip_from.is_some() {
                    continue;
                }
                let name = start.local_name().as_ref().to_string();
                if let Some(state) = item.as_mut().filter(|s| depth == s.depth + 1) {
                    if is_price_element(&name) {
                        skip_from = Some(depth);
                        continue;
                    }
                    if name == "Description" {
                        insert_prices(&mut writer, state).map_err(io_err)?;
                    }
                }
                if name == "Item" {
                    item = Some(ItemState {
                        depth,
                        price: item_prices.get(item_index).copied().flatten(),
                        prefix: prefix_of(&start),
                        inserted: false,
                    });
                    item_index += 1;
                }
                in_dp = name == "DP";
                let start = if depth == 1 {
                    retarget_namespaces(&start, source_dp)
                } else {
                    start
                };
                writer.write_event(Event::Start(start)).map_err(io_err)?;
            }
            Event::Empty(empty) => {
                if skip_from.is_some() {
                    continue;
                }
                let name = empty.local_name().as_ref().to_string();
                if let Some(state) = item.as_mut().filter(|s| depth == s.depth) {
                    if is_price_element(&name) {
                        continue;
                    }
                    if name == "Description" {
                        insert_prices(&mut writer, state).map_err(io_err)?;
                    }
                }
                if name == "Item" {
                    item_index += 1;
                }
                writer.write_event(Event::Empty(empty)).map_err(io_err)?;
            }
            Event::End(end) => {
                if let Some(from) = skip_from {
                    if depth == from {
                        skip_from = None;
                    }
                    depth -= 1;
                    continue;
                }
                let name = end.local_name().as_ref().to_string();
                let closes_item = item.as_ref().is_some_and(|s| depth == s.depth);
                if closes_item && let Some(state) = item.as_mut() {
                    insert_prices(&mut writer, state).map_err(io_err)?;
                }
                writer.write_event(Event::End(end)).map_err(io_err)?;
                if let Some(state) = item.as_mut().filter(|s| depth == s.depth + 1)
                    && name == "QU"
                {
                    insert_prices(&mut writer, state).map_err(io_err)?;
                }
                if closes_item {
                    item = None;
                }
                in_dp = false;
                depth -= 1;
            }
            Event::Text(text) => {
                if skip_from.is_some() {
                    continue;
                }
                let event = if in_dp && !text.xml10_content().trim().is_empty() {
                    Event::Text(BytesText::new("84"))
                } else {
                    Event::Text(text)
                };
                writer.write_event(event).map_err(io_err)?;
            }
            other => {
                if skip_from.is_none() {
                    writer.write_event(other).map_err(io_err)?;
                }
            }
        }
    }

    String::from_utf8(writer.into_inner()).map_err(|e| GaebError::Write(e.to_string()))
}

fn is_price_element(local_name: &str) -> bool {
    matches!(local_name, "UP" | "IT" | "UPBkdn") || local_name.starts_with("UPComp")
}

fn prefix_of(start: &BytesStart<'_>) -> String {
    let qname = start.name();
    let local = start.local_name();
    qname
        .as_ref()
        .strip_suffix(local.as_ref())
        .unwrap_or_default()
        .to_string()
}

/// Schreibt `UP` und `IT` genau einmal pro Item (nur wenn ein Preis vorliegt).
fn insert_prices(writer: &mut Writer<Vec<u8>>, state: &mut ItemState) -> std::io::Result<()> {
    if std::mem::replace(&mut state.inserted, true) {
        return Ok(());
    }
    let Some((ep, gp)) = state.price else {
        return Ok(());
    };
    for (tag, value) in [("UP", format_up(ep)), ("IT", format_amount(gp))] {
        let name = format!("{}{tag}", state.prefix);
        writer.write_event(Event::Start(BytesStart::new(name.clone())))?;
        writer.write_event(Event::Text(BytesText::new(&value)))?;
        writer.write_event(Event::End(BytesEnd::new(name)))?;
    }
    Ok(())
}

/// Namespace (`…/DA83/3.3`) und `schemaLocation` des Wurzelelements auf DA84 umstellen.
fn retarget_namespaces<'a>(start: &BytesStart<'a>, source_dp: u8) -> BytesStart<'a> {
    let from = format!("DA{source_dp}");
    let to = format!("DA{}", Phase::X84.dp());
    let mut out = BytesStart::new(start.name().as_ref().to_string());
    for attr in start.attributes().flatten() {
        let key = attr.key.as_ref().to_string();
        let retarget =
            key == "xmlns" || key.starts_with("xmlns:") || key.ends_with("schemaLocation");
        let value = if retarget {
            Cow::Owned(attr.value.replace(&from, &to))
        } else {
            attr.value
        };
        out.push_attribute(Attribute {
            key: QName(&key),
            value,
        });
    }
    out
}

/// Einheitspreis: mindestens 2, höchstens 3 Nachkommastellen.
fn format_up(value: Decimal) -> String {
    let mut v = value.round_dp(3).normalize();
    if v.scale() < 2 {
        v.rescale(2);
    }
    v.to_string()
}

/// Beträge immer mit 2 Nachkommastellen, kaufmännisch gerundet.
fn format_amount(value: Decimal) -> String {
    let mut v = value.round_dp_with_strategy(2, lv_model::RoundingStrategy::MidpointAwayFromZero);
    v.rescale(2);
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn price_formats() {
        assert_eq!(format_up(dec!(45.5)), "45.50");
        assert_eq!(format_up(dec!(12)), "12.00");
        assert_eq!(format_up(dec!(12.345)), "12.345");
        assert_eq!(format_up(dec!(12.34567)), "12.346");
        assert_eq!(format_amount(dec!(37.035)), "37.04");
        assert_eq!(format_amount(dec!(5460)), "5460.00");
    }
}
