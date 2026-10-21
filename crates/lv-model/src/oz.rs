use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// GAEB erlaubt bis zu vier Titelebenen plus Position und Index.
const MAX_DEPTH: usize = 6;

/// Ordnungszahl eines Titels oder einer Position, z. B. `01.02.0030`.
///
/// Führende Nullen bleiben erhalten (sie sind Teil der OZ-Maske), verglichen wird numerisch.
/// Achtung: Auch ein Datum wie `12.03.2024` ist syntaktisch eine gültige OZ – ob ein Token
/// wirklich eine OZ ist, entscheidet der Parser anhand von Spalte und Maske.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Oz {
    parts: Vec<String>,
}

impl Oz {
    /// Akzeptiert `01.02.0030`, `1.2.30` und `01.02.0030.`; jeder Teil besteht aus Ziffern,
    /// optional gefolgt von einem Index-Buchstaben.
    pub fn parse(raw: &str) -> Option<Oz> {
        let trimmed = raw.trim().trim_end_matches('.');
        if trimmed.is_empty() {
            return None;
        }
        let parts: Vec<String> = trimmed.split('.').map(str::to_string).collect();
        let valid = parts.len() <= MAX_DEPTH && parts.iter().all(|p| is_valid_part(p));
        valid.then_some(Oz { parts })
    }

    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    pub fn depth(&self) -> usize {
        self.parts.len()
    }

    pub fn parent(&self) -> Option<Oz> {
        (self.parts.len() > 1).then(|| Oz {
            parts: self.parts[..self.parts.len() - 1].to_vec(),
        })
    }

    /// `01.02` ist Vorfahre von `01.02.0030` – auch von `1.2.30`, weil numerisch verglichen wird.
    pub fn is_ancestor_of(&self, other: &Oz) -> bool {
        self.parts.len() < other.parts.len()
            && self
                .parts
                .iter()
                .zip(&other.parts)
                .all(|(a, b)| cmp_part(a, b) == Ordering::Equal)
    }

    pub fn mask(&self) -> OzMask {
        OzMask(self.parts.iter().map(|p| p.len() as u8).collect())
    }
}

fn is_valid_part(part: &str) -> bool {
    let digits = part.chars().take_while(char::is_ascii_digit).count();
    let rest = &part[digits..];
    digits > 0
        && digits <= 8
        && rest.chars().count() <= 1
        && rest.chars().all(|c| c.is_ascii_alphabetic())
}

fn cmp_part(a: &str, b: &str) -> Ordering {
    let split = |s: &str| {
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        // Gültige Teile haben höchstens 8 Ziffern, passt sicher in u64.
        (
            s[..digits].parse::<u64>().unwrap_or(0),
            s[digits..].to_ascii_uppercase(),
        )
    };
    split(a).cmp(&split(b))
}

impl Ord for Oz {
    fn cmp(&self, other: &Self) -> Ordering {
        for (a, b) in self.parts.iter().zip(&other.parts) {
            match cmp_part(a, b) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }
        self.parts
            .len()
            .cmp(&other.parts.len())
            // Numerisch gleich, aber anders geschrieben ("1.2.30" vs. "01.02.0030"): nach Text
            // ordnen, damit `Ord` konsistent zu `Eq` bleibt (wichtig für BTreeMap & Co.).
            .then_with(|| self.parts.cmp(&other.parts))
    }
}

impl PartialOrd for Oz {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Oz {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.parts.join("."))
    }
}

impl TryFrom<String> for Oz {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Oz::parse(&value).ok_or_else(|| format!("ungültige OZ: {value:?}"))
    }
}

impl From<Oz> for String {
    fn from(oz: Oz) -> Self {
        oz.to_string()
    }
}

/// Stellenzahl je OZ-Ebene, z. B. `[2, 2, 4]` für `01.02.0030`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OzMask(pub Vec<u8>);

impl OzMask {
    /// Passt eine Position (volle Tiefe) oder ein Titel (Präfix) zur Maske?
    pub fn matches(&self, oz: &Oz) -> bool {
        oz.depth() <= self.0.len()
            && oz
                .parts()
                .iter()
                .zip(&self.0)
                .all(|(p, &len)| p.chars().take_while(char::is_ascii_digit).count() == len as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oz(s: &str) -> Oz {
        Oz::parse(s).unwrap()
    }

    #[test]
    fn parses_and_displays() {
        assert_eq!(oz("01.02.0030").to_string(), "01.02.0030");
        assert_eq!(oz("01.02.0030.").to_string(), "01.02.0030");
        assert_eq!(oz("1.2.30A").parts(), ["1", "2", "30A"]);
        for bad in [
            "",
            "Pos",
            "01..30",
            "01.02.003X5",
            "1.2.3.4.5.6.7",
            "01.02.12345678901",
        ] {
            assert_eq!(Oz::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn orders_numerically() {
        assert!(oz("01.02.0030") < oz("01.02.0100"));
        assert!(oz("1.10") > oz("1.9"));
        assert!(oz("01.02") < oz("01.02.0010"));
        assert!(oz("01.02.0030") < oz("01.02.0030A"));
        // Andere Schreibweise: numerisch eingeordnet, aber nicht gleich.
        assert_ne!(oz("1.2.30"), oz("01.02.0030"));
        assert_ne!(oz("1.2.30").cmp(&oz("01.02.0030")), Ordering::Equal);
        assert!(oz("1.2.30") < oz("01.02.0031") && oz("1.2.30") > oz("01.02.0029"));
    }

    #[test]
    fn hierarchy() {
        assert!(oz("01.02").is_ancestor_of(&oz("1.2.30")));
        assert!(!oz("01.03").is_ancestor_of(&oz("01.02.0030")));
        assert!(!oz("01.02.0030").is_ancestor_of(&oz("01.02.0030")));
        assert_eq!(oz("01.02.0030").parent(), Some(oz("01.02")));
        assert_eq!(oz("01").parent(), None);
    }

    #[test]
    fn mask_matching() {
        let mask = oz("01.02.0030").mask();
        assert_eq!(mask, OzMask(vec![2, 2, 4]));
        assert!(mask.matches(&oz("03.01.0010")));
        assert!(mask.matches(&oz("03.01")));
        assert!(!mask.matches(&oz("3.1.10")));
        assert!(!mask.matches(&oz("03.01.0010.01")));
    }

    #[test]
    fn serde_roundtrip_as_string() {
        let json = serde_json::to_string(&oz("01.02.0030")).unwrap();
        assert_eq!(json, "\"01.02.0030\"");
        let back: Oz = serde_json::from_str(&json).unwrap();
        assert_eq!(back, oz("01.02.0030"));
        assert!(serde_json::from_str::<Oz>("\"Pos\"").is_err());
    }
}
