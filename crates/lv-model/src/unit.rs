use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Normalisierte Mengeneinheit. Der Rohtext aus dem Dokument bleibt im [`crate::Field`] erhalten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    M,
    M2,
    M3,
    Stk,
    T,
    Kg,
    L,
    H,
    Tag,
    Wo,
    Psch,
    /// Stück × Woche, z. B. Bauzaunelemente.
    StWo,
    /// Meter × Woche, z. B. Absperrungen.
    MWo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Length,
    Area,
    Volume,
    Count,
    Mass,
    Time,
    Lump,
    CountTime,
    LengthTime,
}

/// Ob und wie sich Preise zwischen zwei Einheiten vergleichen lassen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    /// Menge in `to` = Menge in `from` × `factor`.
    Scaled(Decimal),
    /// Masse ↔ Volumen: nur mit Dichte-Attribut vergleichbar (z. B. Schotter t ↔ m³).
    NeedsDensity,
    Incompatible,
}

impl Unit {
    /// Erkennt die gängigen Schreibweisen aus LVs, z. B. `qm`, `m²`, `cbm`, `Stck`, `lfm`, `psch`.
    pub fn parse(raw: &str) -> Option<Unit> {
        let key: String = raw
            .chars()
            .filter(|c| !c.is_whitespace() && !matches!(c, '.' | '^' | '*' | '×'))
            .flat_map(char::to_lowercase)
            .map(|c| match c {
                '²' => '2',
                '³' => '3',
                _ => c,
            })
            .collect();
        let unit = match key.as_str() {
            "m" | "lfm" | "lfdm" | "mtr" | "meter" => Unit::M,
            "m2" | "qm" => Unit::M2,
            "m3" | "cbm" | "kbm" => Unit::M3,
            "st" | "stk" | "stck" | "stück" | "stueck" | "pcs" => Unit::Stk,
            "t" | "to" | "tonne" | "tonnen" => Unit::T,
            "kg" => Unit::Kg,
            "l" | "ltr" | "liter" => Unit::L,
            "h" | "std" | "stde" | "stunde" | "stunden" => Unit::H,
            "d" | "tag" | "tage" | "tg" | "at" => Unit::Tag,
            "wo" | "woche" | "wochen" => Unit::Wo,
            "psch" | "pschl" | "pausch" | "pauschal" | "pa" | "ps" => Unit::Psch,
            "stwo" | "stkwo" | "stxwo" => Unit::StWo,
            "mwo" | "mxwo" => Unit::MWo,
            _ => return None,
        };
        Some(unit)
    }

    /// ASCII-Kürzel, wie es auch in GAEB-Dateien üblich ist.
    pub fn symbol(self) -> &'static str {
        match self {
            Unit::M => "m",
            Unit::M2 => "m2",
            Unit::M3 => "m3",
            Unit::Stk => "St",
            Unit::T => "t",
            Unit::Kg => "kg",
            Unit::L => "l",
            Unit::H => "h",
            Unit::Tag => "Tag",
            Unit::Wo => "Wo",
            Unit::Psch => "psch",
            Unit::StWo => "StWo",
            Unit::MWo => "mWo",
        }
    }

    pub fn dimension(self) -> Dimension {
        match self {
            Unit::M => Dimension::Length,
            Unit::M2 => Dimension::Area,
            Unit::M3 | Unit::L => Dimension::Volume,
            Unit::Stk => Dimension::Count,
            Unit::T | Unit::Kg => Dimension::Mass,
            // Stunden und Tage werden bewusst nicht ineinander umgerechnet (Tageslänge variiert).
            Unit::H | Unit::Tag | Unit::Wo => Dimension::Time,
            Unit::Psch => Dimension::Lump,
            Unit::StWo => Dimension::CountTime,
            Unit::MWo => Dimension::LengthTime,
        }
    }

    /// Faktor zur Basiseinheit der Dimension, falls exakt umrechenbar.
    fn base_factor(self) -> Option<Decimal> {
        match self {
            Unit::L | Unit::Kg => Some(Decimal::new(1, 3)),
            Unit::H | Unit::Tag | Unit::Wo => None,
            _ => Some(Decimal::ONE),
        }
    }

    pub fn compatibility(self, to: Unit) -> Compatibility {
        if self == to {
            return Compatibility::Scaled(Decimal::ONE);
        }
        let (from_dim, to_dim) = (self.dimension(), to.dimension());
        if from_dim == to_dim {
            return match (self.base_factor(), to.base_factor()) {
                (Some(f), Some(t)) => Compatibility::Scaled(f / t),
                _ => Compatibility::Incompatible,
            };
        }
        match (from_dim, to_dim) {
            (Dimension::Mass, Dimension::Volume) | (Dimension::Volume, Dimension::Mass) => {
                Compatibility::NeedsDensity
            }
            _ => Compatibility::Incompatible,
        }
    }
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.symbol())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn parses_common_lv_spellings() {
        let cases = [
            ("qm", Unit::M2),
            ("m²", Unit::M2),
            ("M2", Unit::M2),
            ("cbm", Unit::M3),
            ("m³", Unit::M3),
            ("Stck", Unit::Stk),
            ("St.", Unit::Stk),
            ("Stück", Unit::Stk),
            ("lfm", Unit::M),
            ("lfd. m", Unit::M),
            ("psch", Unit::Psch),
            ("PA", Unit::Psch),
            ("to", Unit::T),
            ("Std", Unit::H),
            ("St*Wo", Unit::StWo),
            ("St x Wo", Unit::StWo),
        ];
        for (raw, expected) in cases {
            assert_eq!(Unit::parse(raw), Some(expected), "{raw}");
        }
        assert_eq!(Unit::parse("Betonpflaster"), None);
        assert_eq!(Unit::parse(""), None);
    }

    #[test]
    fn compatibility_rules() {
        assert_eq!(
            Unit::M2.compatibility(Unit::M2),
            Compatibility::Scaled(dec!(1))
        );
        assert_eq!(
            Unit::Kg.compatibility(Unit::T),
            Compatibility::Scaled(dec!(0.001))
        );
        assert_eq!(
            Unit::T.compatibility(Unit::Kg),
            Compatibility::Scaled(dec!(1000))
        );
        assert_eq!(Unit::T.compatibility(Unit::M3), Compatibility::NeedsDensity);
        assert_eq!(
            Unit::H.compatibility(Unit::Tag),
            Compatibility::Incompatible
        );
        assert_eq!(Unit::M2.compatibility(Unit::M), Compatibility::Incompatible);
    }
}
