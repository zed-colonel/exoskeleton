//! Event domain types and store trait for the Event Ledger.
//!
//! The Event Ledger is an append-only audit trail of everything notable that
//! happens during vessel operation. Each entry captures an event type, an
//! optional reference to a detailed artifact, and a human-readable summary.
//!
//! Events are written by the master loop (Sprint 5), the LLM handler (Sprint 4),
//! thread execution (Sprint 6), and the Align step (Sprint 8). Sprint 2 provides
//! the storage layer; later sprints are the producers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, LedgerEntryId, TickId};
use crate::ExoError;

/// One entry in the Event Ledger.
///
/// The Event Ledger is an append-only audit trail of everything notable that
/// happens during vessel operation. Each entry captures an event type, an
/// optional reference to a detailed artifact, and a human-readable summary.
///
/// Events are written by the master loop (Sprint 5), the LLM handler (Sprint 4),
/// thread execution (Sprint 6), and the Align step (Sprint 8). Sprint 2 provides
/// the storage layer; later sprints are the producers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEntry {
    /// Unique identity of this event entry.
    pub id: LedgerEntryId,
    /// Which tick produced this event, if any. `None` for out-of-tick events
    /// (e.g., VesselStarted, VesselStopped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_id: Option<TickId>,
    /// What kind of event this is.
    pub event_type: EventType,
    /// Reference to a detailed artifact, if one exists.
    /// Not all events produce artifacts — simple lifecycle events (start/stop)
    /// are fully captured by the summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<ArtifactId>,
    /// Human-readable description of the event.
    pub summary: String,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
}

/// Classification of events in the Event Ledger.
///
/// Event types correspond to the major operations in the vessel lifecycle and
/// PODAARA loop. Each type may appear with different payloads and summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// Vessel started (both engines bootstrapped).
    VesselStarted,
    /// Vessel stopped (clean shutdown).
    VesselStopped,
    /// A cognitive tick started.
    TickStarted,
    /// A cognitive tick completed.
    TickCompleted,
    /// An action was executed via the Act step (Tool AQ).
    ActionExecuted,
    /// An LLM was called (Cognitive AQ).
    LlmCalled,
    /// A cognitive thread ran and produced output.
    ThreadRan,
    /// A relationship was updated via the Align step.
    RelationshipUpdated,
    /// Budget was consumed (tokens, cost, time).
    BudgetConsumed,
    /// An error occurred.
    Error,
}

/// Append-only event ledger for the vessel's audit trail.
///
/// Events are written as they occur — once written, they are never modified or
/// deleted. The ledger is the primary audit record for "what happened and when."
///
/// Combined with the Artifact Store and both AQ WALs, the Event Ledger enables
/// full operational replay (I3).
pub trait EventLedger: Send + Sync {
    /// Append a new event entry to the ledger. Returns the entry's ID.
    ///
    /// The entry's `id` field should already be populated (caller creates the
    /// `LedgerEntryId`). If an entry with the same ID already exists, this
    /// returns `ExoError::Storage` — IDs are unique (UUID v4 collisions are
    /// effectively impossible, so this indicates a bug).
    fn append(&self, entry: &EventEntry) -> Result<LedgerEntryId, ExoError>;

    /// Get the N most recent events, newest first.
    fn recent(&self, limit: usize) -> Result<Vec<EventEntry>, ExoError>;

    /// Get all events associated with a specific tick.
    ///
    /// Returns events in chronological order (timestamp ASC) within the tick.
    fn for_tick(&self, tick_id: TickId) -> Result<Vec<EventEntry>, ExoError>;

    /// Get events of a specific type, newest first.
    fn by_type(&self, event_type: EventType, limit: usize) -> Result<Vec<EventEntry>, ExoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_all_variants_roundtrip() {
        let variants = [
            EventType::VesselStarted,
            EventType::VesselStopped,
            EventType::TickStarted,
            EventType::TickCompleted,
            EventType::ActionExecuted,
            EventType::LlmCalled,
            EventType::ThreadRan,
            EventType::RelationshipUpdated,
            EventType::BudgetConsumed,
            EventType::Error,
        ];
        for event_type in &variants {
            let json = serde_json::to_string(event_type).unwrap();
            let parsed: EventType = serde_json::from_str(&json).unwrap();
            assert_eq!(*event_type, parsed);
        }
    }

    #[test]
    fn event_type_snake_case() {
        assert_eq!(
            serde_json::to_string(&EventType::VesselStarted).unwrap(),
            "\"vessel_started\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::LlmCalled).unwrap(),
            "\"llm_called\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::RelationshipUpdated).unwrap(),
            "\"relationship_updated\""
        );
    }

    #[test]
    fn event_entry_json_roundtrip_full() {
        let entry = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(TickId::new()),
            event_type: EventType::ActionExecuted,
            payload_ref: Some(ArtifactId::from_content(b"receipt")),
            summary: "Wrote file /tmp/output.txt".into(),
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: EventEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn event_entry_json_roundtrip_minimal() {
        let entry = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::VesselStarted,
            payload_ref: None,
            summary: "Vessel started".into(),
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: EventEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn event_entry_optional_fields_omitted() {
        let entry = EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::VesselStopped,
            payload_ref: None,
            summary: "Vessel stopped".into(),
            timestamp: Utc::now(),
        };
        let value: serde_json::Value = serde_json::to_value(&entry).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("tick_id"));
        assert!(!obj.contains_key("payload_ref"));
    }
}
