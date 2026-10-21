//! Domänenmodell eines Leistungsverzeichnisses (LV).
//!
//! Fachbegriffe (Titel, Position, Kurztext, Langtext, EP, GP, OZ) bleiben bewusst deutsch,
//! weil es für sie keine eindeutige englische Entsprechung gibt. Das Crate ist WASM-tauglich
//! und wird von Server, Parser und Leptos-Client gemeinsam genutzt.

mod field;
mod lv;
pub mod number;
mod oz;
mod unit;

pub use field::{Field, ReviewIssue};
pub use lv::{
    Lv, LvSource, Node, Position, PositionsArt, REVIEW_THRESHOLD_NUMBER, REVIEW_THRESHOLD_TEXT,
    Titel, Vorbemerkung,
};
pub use oz::{Oz, OzMask};
pub use rust_decimal::Decimal;
pub use unit::{Compatibility, Dimension, Unit};
