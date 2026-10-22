use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

use lv_model::number::{NumberLocale, parse_decimal};
use lv_model::{
    Decimal, Field, Lv, LvSource, Node as LvNode, Oz, OzMask, Position, PositionsArt, ReviewIssue,
    Titel, Unit, Vorbemerkung,
};
use roxmltree::{Document, Node, NodeId};

use crate::text::{plain_text, single_line};
use crate::{GaebError, Phase};

/// Ergebnis eines GAEB-Imports.
#[derive(Debug, Clone)]
pub struct GaebImport {
    pub phase: Phase,
    /// Schema-Version laut `GAEBInfo`, z. B. `"3.3"`.
    pub version: Option<String>,
    pub lv: Lv,
    /// Inhalte, die (noch) nicht übernommen werden, z. B. Zuschlagspositionen.
    pub warnings: Vec<String>,
}

/// Liest GAEB DA XML. Liefert zusätzlich die OZ jedes `Item`-Elements in Dokumentreihenfolge;
/// darüber ordnet der X84-Writer die Preise zu.
pub(crate) fn read(xml: &str) -> Result<(GaebImport, Vec<Option<Oz>>), GaebError> {
    let doc = Document::parse(xml)?;
    let root = doc.root_element();
    if root.tag_name().name() != "GAEB" {
        return Err(GaebError::NotGaeb(root.tag_name().name().to_string()));
    }
    let award = child(root, "Award").ok_or(GaebError::Missing("Award"))?;
    let phase = child_text(award, "DP")
        .and_then(Phase::from_dp)
        .ok_or(GaebError::Missing("Award/DP"))?;
    let boq = child(award, "BoQ").ok_or(GaebError::Missing("Award/BoQ"))?;
    let body = child(boq, "BoQBody").ok_or(GaebError::Missing("BoQ/BoQBody"))?;
    let prj = child(root, "PrjInfo");

    let mut reader = Reader {
        breakdown: Breakdown::read(child(boq, "BoQInfo")),
        ozs: HashMap::new(),
        aln_base: HashMap::new(),
        warnings: Vec::new(),
        unknown: BTreeSet::new(),
    };

    // Erst alle OZ bestimmen: Alternativpositionen verweisen auf ihre Grundposition, die im
    // Dokument auch erst später stehen kann.
    let items: Vec<Node> = doc
        .descendants()
        .filter(|n| is_element(n, "Item"))
        .collect();
    for &item in &items {
        let oz = reader.entry_oz(item).as_deref().and_then(Oz::parse);
        let is_base = child_text(item, "ALNSerNo").is_some_and(|s| s.parse() == Ok(0u32));
        if let (true, Some(group), Some(oz)) = (is_base, child_text(item, "ALNGroupNo"), &oz) {
            reader.aln_base.insert(group.to_string(), oz.clone());
        }
        reader.ozs.insert(item.id(), oz);
    }
    let item_ozs = items.iter().map(|i| reader.ozs[&i.id()].clone()).collect();

    let nodes = reader.body(body);
    for name in std::mem::take(&mut reader.unknown) {
        reader
            .warnings
            .push(format!("Element <{name}> im LV wird ignoriert"));
    }

    let name = prj
        .and_then(|p| child_text(p, "NamePrj"))
        .or_else(|| path(boq, &["BoQInfo", "Name"]).and_then(|n| n.text()))
        .or_else(|| prj.and_then(|p| child_text(p, "LblPrj")))
        .unwrap_or("GAEB-LV");
    let currency = child_text(award, "Cur")
        .or_else(|| prj.and_then(|p| child_text(p, "Cur")))
        .unwrap_or("EUR");

    let lv = Lv {
        name: name.trim().to_string(),
        source: LvSource::Gaeb {
            format: phase.code(),
        },
        currency: currency.to_string(),
        oz_mask: reader.breakdown.mask(),
        nodes,
    };
    let import = GaebImport {
        phase,
        version: path(root, &["GAEBInfo", "Version"])
            .and_then(|v| v.text())
            .map(|v| v.trim().to_string()),
        lv,
        warnings: reader.warnings,
    };
    Ok((import, item_ozs))
}

/// OZ-Gliederung laut `BoQInfo/BoQBkdn`: Stellen je Titelebene und für die Position.
struct Breakdown {
    levels: Vec<usize>,
    item: Option<usize>,
}

impl Breakdown {
    fn read(info: Option<Node<'_, '_>>) -> Self {
        let mut b = Breakdown {
            levels: Vec::new(),
            item: None,
        };
        let entries = info
            .into_iter()
            .flat_map(|i| i.children())
            .filter(|n| is_element(n, "BoQBkdn"));
        for entry in entries {
            let len = child_text(entry, "Length").and_then(|l| l.parse().ok());
            match child_text(entry, "Type") {
                Some("BoQLevel" | "Lot") => b.levels.push(len.unwrap_or(0)),
                Some("Item") => b.item = len,
                _ => {}
            }
        }
        b
    }

    fn mask(&self) -> Option<OzMask> {
        let item = self.item?;
        let lengths: Vec<u8> = self
            .levels
            .iter()
            .chain(std::iter::once(&item))
            .map(|&l| u8::try_from(l).ok().filter(|&l| l > 0))
            .collect::<Option<_>>()?;
        Some(OzMask(lengths))
    }
}

struct Reader {
    breakdown: Breakdown,
    ozs: HashMap<NodeId, Option<Oz>>,
    /// ALN-Gruppe → OZ der Grundposition (`ALNSerNo` 0).
    aln_base: HashMap<String, Oz>,
    warnings: Vec<String>,
    unknown: BTreeSet<String>,
}

impl Reader {
    /// RNoPart-Teile der umgebenden Titel (inkl. `node`, falls selbst ein Titel), aufgefüllt auf
    /// die Stellenzahl der Gliederung: `RNoPart="1"` → `"01"`.
    fn category_parts(&self, node: Node<'_, '_>) -> Vec<String> {
        let mut raw: Vec<&str> = node
            .ancestors()
            .filter(|n| is_element(n, "BoQCtgy"))
            .filter_map(|n| n.attribute("RNoPart"))
            .collect();
        raw.reverse();
        raw.iter()
            .enumerate()
            .map(|(i, part)| pad(part, self.breakdown.levels.get(i).copied()))
            .collect()
    }

    /// OZ (Rohtext) eines Eintrags im Itemlist (`Item`, `MarkupItem`).
    fn entry_oz(&self, node: Node<'_, '_>) -> Option<String> {
        let mut parts = self.category_parts(node);
        parts.push(pad(node.attribute("RNoPart")?, self.breakdown.item));
        if let Some(index) = node
            .attribute("RNoIndex")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if index.chars().all(|c| c.is_ascii_alphabetic()) {
                parts.last_mut()?.push_str(index);
            } else {
                parts.push(index.to_string());
            }
        }
        Some(parts.join("."))
    }

    fn body(&mut self, body: Node<'_, '_>) -> Vec<LvNode> {
        let mut nodes = Vec::new();
        for child in body.children().filter(Node::is_element) {
            match child.tag_name().name() {
                "BoQCtgy" => nodes.push(self.category(child)),
                "Itemlist" => {
                    for entry in child.children().filter(Node::is_element) {
                        nodes.extend(self.entry(entry));
                    }
                }
                other => {
                    self.unknown.insert(other.to_string());
                }
            }
        }
        nodes
    }

    fn category(&mut self, ctgy: Node<'_, '_>) -> LvNode {
        let raw = self.category_parts(ctgy).join(".");
        let label = child(ctgy, "LblTx").map(single_line).unwrap_or_default();
        let children = child(ctgy, "BoQBody")
            .map(|b| self.body(b))
            .unwrap_or_default();
        LvNode::Titel(Titel {
            oz: Field::parsed(Oz::parse(&raw), raw),
            bezeichnung: Field::certain(label),
            children,
        })
    }

    fn entry(&mut self, node: Node<'_, '_>) -> Option<LvNode> {
        match node.tag_name().name() {
            "Item" => Some(LvNode::Position(Box::new(self.item(node)))),
            "Remark" | "PerfDescr" => {
                let text = path(node, &["Description", "CompleteText"])
                    .map(plain_text)
                    .unwrap_or_else(|| plain_text(node));
                Some(LvNode::Vorbemerkung(Vorbemerkung {
                    text: Field::certain(text),
                }))
            }
            "MarkupItem" => {
                let oz = self.entry_oz(node).unwrap_or_else(|| "?".into());
                self.warnings
                    .push(format!("Zuschlagsposition {oz} wird noch nicht übernommen"));
                None
            }
            other => {
                self.unknown.insert(other.to_string());
                None
            }
        }
    }

    fn item(&self, item: Node<'_, '_>) -> Position {
        let raw_oz = self.entry_oz(item).unwrap_or_default();
        let oz = self.ozs.get(&item.id()).cloned().flatten();

        let text = path(item, &["Description", "CompleteText"]);
        let langtext = text
            .and_then(|t| child(t, "DetailTxt"))
            .map(plain_text)
            .unwrap_or_default();
        let kurztext = text
            .and_then(|t| path(t, &["OutlineText", "OutlTxt"]))
            .map(single_line)
            .filter(|s| !s.is_empty())
            .or_else(|| langtext.lines().next().map(str::to_string))
            .unwrap_or_default();

        let alternative =
            child_text(item, "ALNSerNo").is_some_and(|s| s.parse().is_ok_and(|n: u32| n > 0));
        let (art, grund_oz) = if alternative {
            let base = child_text(item, "ALNGroupNo").and_then(|g| self.aln_base.get(g).cloned());
            (PositionsArt::Alternativ, base)
        } else if child(item, "Provis").is_some() {
            (PositionsArt::Bedarf, None)
        } else if child_text(item, "HourIt").is_some_and(is_yes) {
            (PositionsArt::Stundenlohn, None)
        } else {
            (PositionsArt::Normal, None)
        };

        Position {
            oz: Field::parsed(oz, raw_oz),
            kurztext: Field::certain(kurztext),
            langtext: if langtext.is_empty() {
                Field::absent()
            } else {
                Field::certain(langtext)
            },
            menge: decimal(child_text(item, "Qty")),
            einheit: unit(child_text(item, "QU")),
            ep: decimal(child_text(item, "UP")),
            gp: decimal(child_text(item, "IT")),
            art,
            grund_oz,
        }
    }
}

/// Numerische Teile auf die Stellenzahl der Gliederung auffüllen; nicht-numerische unverändert.
fn pad(part: &str, len: Option<usize>) -> String {
    let part = part.trim();
    match len {
        Some(len) if part.len() < len && part.chars().all(|c| c.is_ascii_digit()) => {
            format!("{part:0>len$}")
        }
        _ => part.to_string(),
    }
}

/// XML-Dezimalzahl (`120.000`); einige Programme schreiben trotzdem deutsches Format.
fn decimal(raw: Option<&str>) -> Field<Decimal> {
    raw.map_or_else(Field::absent, |r| {
        let value = Decimal::from_str(r)
            .ok()
            .or_else(|| parse_decimal(r, NumberLocale::German));
        Field::parsed(value, r)
    })
}

fn unit(raw: Option<&str>) -> Field<Unit> {
    raw.map_or_else(Field::absent, |r| {
        let mut field = Field::parsed(Unit::parse(r), r);
        if field.value.is_none() {
            field.issues = vec![ReviewIssue::UnknownUnit];
        }
        field
    })
}

fn is_yes(value: &str) -> bool {
    value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("true") || value == "1"
}

fn is_element(node: &Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| is_element(c, name))
}

fn path<'a, 'i>(node: Node<'a, 'i>, names: &[&str]) -> Option<Node<'a, 'i>> {
    names.iter().try_fold(node, |n, name| child(n, name))
}

fn child_text<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(node, name)?
        .text()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}
