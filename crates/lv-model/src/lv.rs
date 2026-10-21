use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::{Field, Oz, OzMask, ReviewIssue, Unit};

/// Unterhalb dieser Konfidenz gehen Zahlen, OZ und Einheiten ins Review.
pub const REVIEW_THRESHOLD_NUMBER: f32 = 0.9;
/// Texte dürfen unsicherer sein; ein Tippfehler im Langtext ist selten preisrelevant.
pub const REVIEW_THRESHOLD_TEXT: f32 = 0.6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LvSource {
    Pdf {
        pages: u32,
    },
    /// z. B. `"X83"`, `"D83"`
    Gaeb {
        format: String,
    },
    Excel,
    Manual,
}

/// Ein Leistungsverzeichnis als Baum aus Titeln, Positionen und Vorbemerkungen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lv {
    pub name: String,
    pub source: LvSource,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oz_mask: Option<OzMask>,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Titel(Titel),
    Position(Box<Position>),
    Vorbemerkung(Vorbemerkung),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Titel {
    pub oz: Field<Oz>,
    pub bezeichnung: Field<String>,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vorbemerkung {
    pub text: Field<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionsArt {
    #[default]
    Normal,
    /// Bedarfs- bzw. Eventualposition: nur EP, nicht in der Angebotssumme.
    Bedarf,
    /// Alternativ-/Wahlposition zu einer Grundposition: nicht in der Angebotssumme.
    Alternativ,
    Zulage,
    Stundenlohn,
}

impl PositionsArt {
    pub fn counts_towards_total(self) -> bool {
        matches!(
            self,
            PositionsArt::Normal | PositionsArt::Zulage | PositionsArt::Stundenlohn
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub oz: Field<Oz>,
    pub kurztext: Field<String>,
    pub langtext: Field<String>,
    pub menge: Field<Decimal>,
    pub einheit: Field<Unit>,
    /// Einheitspreis netto.
    pub ep: Field<Decimal>,
    /// Gesamtpreis netto.
    pub gp: Field<Decimal>,
    #[serde(default)]
    pub art: PositionsArt,
    /// Bei Alternativ- und Zulagepositionen: OZ der Grundposition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grund_oz: Option<Oz>,
}

impl Position {
    /// Position mit sicherer OZ und Kurztext; alle anderen Felder sind leer.
    pub fn new(oz: Oz, kurztext: impl Into<String>) -> Self {
        Self {
            oz: Field::certain(oz),
            kurztext: Field::certain(kurztext.into()),
            langtext: Field::absent(),
            menge: Field::absent(),
            einheit: Field::absent(),
            ep: Field::absent(),
            gp: Field::absent(),
            art: PositionsArt::Normal,
            grund_oz: None,
        }
    }

    /// Menge × EP, kaufmännisch auf Cent gerundet.
    pub fn computed_gp(&self) -> Option<Decimal> {
        let (menge, ep) = (self.menge.value?, self.ep.value?);
        Some((menge * ep).round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero))
    }

    /// Prüft GP = Menge × EP (Toleranz 1 Cent) und markiert das GP-Feld bei Abweichung.
    /// Fehlt einer der Werte, gibt es nichts zu prüfen.
    pub fn check_arithmetic(&mut self) -> bool {
        let (Some(expected), Some(gp)) = (self.computed_gp(), self.gp.value) else {
            return true;
        };
        let ok = (expected - gp).abs() <= Decimal::new(1, 2);
        if !ok {
            self.gp.flag(ReviewIssue::ArithmeticMismatch);
        }
        ok
    }

    pub fn needs_review(&self) -> bool {
        self.oz.needs_review(REVIEW_THRESHOLD_NUMBER)
            || self.menge.needs_review(REVIEW_THRESHOLD_NUMBER)
            || self.einheit.needs_review(REVIEW_THRESHOLD_NUMBER)
            || self.ep.needs_review(REVIEW_THRESHOLD_NUMBER)
            || self.gp.needs_review(REVIEW_THRESHOLD_NUMBER)
            || self.kurztext.needs_review(REVIEW_THRESHOLD_TEXT)
            || self.langtext.needs_review(REVIEW_THRESHOLD_TEXT)
    }
}

impl Lv {
    /// Alle Positionen in Dokumentreihenfolge.
    pub fn positions(&self) -> impl Iterator<Item = &Position> {
        Positions {
            stack: vec![self.nodes.iter()],
        }
    }

    /// Angebotssumme netto: GP (ersatzweise Menge × EP) aller Positionen, die in die Summe
    /// eingehen. Positionen ohne Preis zählen mit 0.
    pub fn total(&self) -> Decimal {
        self.positions()
            .filter(|p| p.art.counts_towards_total())
            .filter_map(|p| p.gp.value.or_else(|| p.computed_gp()))
            .sum()
    }
}

struct Positions<'a> {
    stack: Vec<std::slice::Iter<'a, Node>>,
}

impl<'a> Iterator for Positions<'a> {
    type Item = &'a Position;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(iter) = self.stack.last_mut() {
            match iter.next() {
                Some(Node::Position(p)) => return Some(p.as_ref()),
                Some(Node::Titel(t)) => self.stack.push(t.children.iter()),
                Some(Node::Vorbemerkung(_)) => {}
                None => {
                    self.stack.pop();
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn oz(s: &str) -> Oz {
        Oz::parse(s).unwrap()
    }

    fn priced(o: &str, menge: Decimal, ep: Decimal, art: PositionsArt) -> Node {
        let mut p = Position::new(oz(o), format!("Position {o}"));
        p.menge = Field::certain(menge);
        p.einheit = Field::certain(Unit::M2);
        p.ep = Field::certain(ep);
        p.art = art;
        Node::Position(Box::new(p))
    }

    fn sample() -> Lv {
        Lv {
            name: "Außenanlagen Kita".into(),
            source: LvSource::Gaeb {
                format: "X83".into(),
            },
            currency: "EUR".into(),
            oz_mask: Some(OzMask(vec![2, 2, 4])),
            nodes: vec![
                Node::Vorbemerkung(Vorbemerkung {
                    text: Field::certain("Allgemeine Vorbemerkungen".into()),
                }),
                Node::Titel(Titel {
                    oz: Field::certain(oz("01")),
                    bezeichnung: Field::certain("Pflasterarbeiten".into()),
                    children: vec![
                        Node::Titel(Titel {
                            oz: Field::certain(oz("01.01")),
                            bezeichnung: Field::certain("Wege".into()),
                            children: vec![
                                priced("01.01.0010", dec!(120), dec!(45.50), PositionsArt::Normal),
                                priced("01.01.0020", dec!(10), dec!(99), PositionsArt::Bedarf),
                            ],
                        }),
                        priced("01.0030", dec!(3), dec!(12.345), PositionsArt::Normal),
                    ],
                }),
            ],
        }
    }

    #[test]
    fn positions_in_document_order() {
        let lv = sample();
        let ozs: Vec<String> = lv
            .positions()
            .map(|p| p.oz.value.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(ozs, ["01.01.0010", "01.01.0020", "01.0030"]);
    }

    #[test]
    fn total_excludes_bedarf_and_rounds_per_position() {
        // 120 × 45,50 = 5460,00; 3 × 12,345 = 37,035 → 37,04; Bedarf zählt nicht.
        assert_eq!(sample().total(), dec!(5497.04));
    }

    #[test]
    fn arithmetic_check_flags_gp() {
        let mut p = Position::new(oz("01.01.0010"), "Pflaster");
        p.menge = Field::certain(dec!(120));
        p.ep = Field::certain(dec!(45.50));
        p.gp = Field::certain(dec!(5460.00));
        assert!(p.check_arithmetic());
        assert!(!p.needs_review());

        p.gp = Field::certain(dec!(5406.00));
        assert!(!p.check_arithmetic());
        assert_eq!(p.gp.issues, [ReviewIssue::ArithmeticMismatch]);
        assert!(p.needs_review());
    }

    #[test]
    fn serde_roundtrip() {
        let lv = sample();
        let json = serde_json::to_value(&lv).unwrap();
        assert_eq!(json["nodes"][1]["type"], "titel");
        assert_eq!(
            json["nodes"][1]["children"][0]["children"][0]["type"],
            "position"
        );
        assert_eq!(
            json["nodes"][1]["children"][0]["children"][0]["ep"]["value"],
            "45.50"
        );
        let back: Lv = serde_json::from_value(json).unwrap();
        assert_eq!(back, lv);
    }
}
