/// GAEB-Datenaustauschphase (Element `DP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Leistungsbeschreibung
    X81,
    /// Kostenanschlag
    X82,
    /// Angebotsaufforderung (LV ohne Preise vom Auftraggeber)
    X83,
    /// Angebotsabgabe (unsere Preise)
    X84,
    /// Nebenangebot
    X85,
    /// Auftragserteilung
    X86,
    Other(u8),
}

impl Phase {
    pub fn from_dp(dp: &str) -> Option<Phase> {
        let n: u8 = dp.trim().parse().ok()?;
        Some(match n {
            81 => Phase::X81,
            82 => Phase::X82,
            83 => Phase::X83,
            84 => Phase::X84,
            85 => Phase::X85,
            86 => Phase::X86,
            other => Phase::Other(other),
        })
    }

    pub fn dp(self) -> u8 {
        match self {
            Phase::X81 => 81,
            Phase::X82 => 82,
            Phase::X83 => 83,
            Phase::X84 => 84,
            Phase::X85 => 85,
            Phase::X86 => 86,
            Phase::Other(n) => n,
        }
    }

    /// Dateikürzel, z. B. `"X83"`.
    pub fn code(self) -> String {
        format!("X{}", self.dp())
    }
}
