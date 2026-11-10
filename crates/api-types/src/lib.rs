//! Typen, die Server und Client teilen – vor allem die Live-Events, mit denen das UI jederzeit
//! zeigt, was das System gerade tut.
//!
//! Die Events werden per SSE an `/jobs/{id}/events` gestreamt. `seq` ist monoton steigend und
//! dient als `Last-Event-ID`, damit ein Client nach einem Verbindungsabbruch lückenlos weiterliest.

pub mod client;

use chrono::{DateTime, Utc};
use lv_model::{Decimal, Oz, Unit};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    LvImport,
    /// Einlesen historischer, bepreister LVs (niedrige Priorität, läuft nachts).
    Backfill,
    Pricing,
    MaterialExtraction,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Verarbeitungsstufen eines Dokuments, in dieser Reihenfolge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Triage,
    Render,
    Ocr,
    VlmFix,
    Structure,
    LlmResolve,
    Embed,
    Price,
    MaterialExtract,
    Export,
}

/// Zustand einer Seite; das UI färbt danach den Thumbnail-Streifen ein.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    Queued,
    TextLayer,
    Ocr,
    Vlm,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuPhase {
    Idle,
    /// OCR, Layout, Tabellen, VLM.
    Vision,
    Llm,
    Switching,
}

/// Kompakte Vorschau einer erkannten Position, solange das LV noch verarbeitet wird.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionPreview {
    /// Laufende Nummer im Dokument; stabiler Schlüssel fürs Upsert im UI, auch ohne OZ.
    pub index: u32,
    pub oz: Option<Oz>,
    pub kurztext: String,
    pub menge: Option<Decimal>,
    pub einheit: Option<Unit>,
    pub needs_review: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEvent {
    pub seq: i64,
    pub job_id: JobId,
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub body: EventBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventBody {
    StageProgress {
        stage: Stage,
        done: u32,
        total: u32,
        eta_s: Option<u32>,
    },
    PageState {
        page: u32,
        status: PageStatus,
        mean_conf: Option<f32>,
    },
    /// Neue oder aktualisierte Positionen; `provisional`, solange die Folgeseite noch fehlt.
    PositionsUpserted {
        items: Vec<PositionPreview>,
        provisional: bool,
    },
    PriceSuggested {
        position_index: u32,
        ep: Decimal,
        conf: f32,
    },
    /// Die GPU wechselt die Phase oder der Job wartet auf sie (z. B. weil der Backfill läuft).
    GpuPhaseChanged {
        phase: GpuPhase,
        waiting_for: Option<JobId>,
    },
    Warning {
        code: String,
        page: Option<u32>,
        message: String,
    },
    JobFinished {
        status: JobStatus,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_is_flat_json_with_tag() {
        let ev = JobEvent {
            seq: 42,
            job_id: JobId(Uuid::from_u128(1)),
            ts: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
            body: EventBody::StageProgress {
                stage: Stage::Ocr,
                done: 3,
                total: 40,
                eta_s: Some(55),
            },
        };
        let value = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            value,
            json!({
                "seq": 42,
                "job_id": "00000000-0000-0000-0000-000000000001",
                "ts": "2025-10-09T08:53:20Z",
                "event": "stage_progress",
                "stage": "ocr",
                "done": 3,
                "total": 40,
                "eta_s": 55
            })
        );
        let back: JobEvent = serde_json::from_value(value).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn position_preview_roundtrip() {
        let ev = EventBody::PositionsUpserted {
            items: vec![PositionPreview {
                index: 0,
                oz: Oz::parse("01.02.0030"),
                kurztext: "Betonpflaster 20x10x8 liefern und verlegen".into(),
                menge: Some(Decimal::new(125_000, 2)),
                einheit: Some(Unit::M2),
                needs_review: false,
            }],
            provisional: true,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"oz\":\"01.02.0030\""), "{json}");
        assert!(json.contains("\"menge\":\"1250.00\""), "{json}");
        assert_eq!(serde_json::from_str::<EventBody>(&json).unwrap(), ev);
    }
}
