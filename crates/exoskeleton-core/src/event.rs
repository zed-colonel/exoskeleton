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
use crate::snapshot::StateSnapshot;
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
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
    /// A message was received via the inbox (D2).
    MessageReceived,
    /// A snapshot fork was created from this vessel (E3-S3).
    VesselForked,
    /// Episodic memory entries were evicted to maintain capacity (E1-S3).
    EpisodicEvicted,
    /// The vessel produced a reply to a user message (OA-S1).
    VesselResponseSent,
    /// An action was blocked by the Align step and the vessel is requesting
    /// capability escalation. Details in payload_ref artifact.
    CapabilityRequest,
    /// A watch condition was triggered during the Perceive step.
    /// Payload artifact contains the watch definition and trigger value.
    WatchTriggered,
    /// A Meta-Cognition thread proposed a charter modification.
    /// Payload artifact contains the CharterProposal with current/proposed text.
    CharterProposal,
    /// An error occurred.
    Error,
}

/// A real-time event emitted by the kernel for external observers.
///
/// Lighter than `EventEntry` — carries enough context for UI updates without
/// requiring artifact retrieval. Observers needing full detail can follow up
/// with REST calls using the embedded IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct LiveEvent {
    /// Event type tag (matches EventType variants for consistency).
    pub event_type: EventType,
    /// Tick number this event belongs to (None for system-level events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_number: Option<u64>,
    /// Human-readable summary.
    pub summary: String,
    /// UTC timestamp.
    pub timestamp: DateTime<Utc>,
    /// Optional snapshot of current vessel status (included on tick_completed
    /// and vessel_started events to avoid a follow-up REST call).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<StateSnapshot>,
}

/// Payload stored as a JSON artifact for CapabilityRequest events.
///
/// Contains the details of what was blocked and why. The operator can use
/// this information to decide whether to adjust AlignConfig.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
pub struct CapabilityRequestPayload {
    /// The tool name that was blocked (e.g., "discord", "webhook.send").
    pub capability: String,
    /// The reason the action was blocked (from the Align step).
    pub reason: String,
    /// The rationale the LLM provided for wanting to use this tool.
    pub context: String,
    /// Whether this request has been acknowledged by an operator.
    #[serde(default)]
    pub acknowledged: bool,
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
            EventType::MessageReceived,
            EventType::VesselForked,
            EventType::EpisodicEvicted,
            EventType::VesselResponseSent,
            EventType::CapabilityRequest,
            EventType::WatchTriggered,
            EventType::CharterProposal,
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

    // ── OA-T8: VesselResponseSent serde roundtrip ──

    #[test]
    fn event_type_vessel_response_sent_roundtrip() {
        let json = serde_json::to_string(&EventType::VesselResponseSent).unwrap();
        assert_eq!(json, "\"vessel_response_sent\"");
        let parsed: EventType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, EventType::VesselResponseSent);
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

    // ── LiveEvent Tests (D2: T-1, T-2, T-3) ──

    #[test]
    fn live_event_serde_roundtrip() {
        let event = LiveEvent {
            event_type: EventType::TickStarted,
            tick_number: Some(42),
            summary: "Tick 42 started".into(),
            timestamp: Utc::now(),
            snapshot: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: LiveEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn live_event_with_snapshot_serde() {
        use crate::id::VesselId;
        use crate::snapshot::{BudgetStatus, VesselStatus};

        let snapshot = StateSnapshot {
            vessel_id: VesselId::new(),
            tick_number: 42,
            mission: "test".into(),
            plan: None,
            status: VesselStatus::Idle,
            working_memory: crate::working_memory::WorkingMemory::new(),
            thread_summaries: Vec::new(),
            relationship_snapshot_ref: None,
            budget_status: BudgetStatus::unlimited(),
            last_action_summary: None,
            started_at: Some(Utc::now()),
            updated_at: Utc::now(),
        };
        let event = LiveEvent {
            event_type: EventType::TickCompleted,
            tick_number: Some(42),
            summary: "Tick 42 completed".into(),
            timestamp: Utc::now(),
            snapshot: Some(snapshot),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: LiveEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, parsed);
        assert!(parsed.snapshot.is_some());
    }

    #[test]
    fn live_event_without_snapshot_omits_field() {
        let event = LiveEvent {
            event_type: EventType::ActionExecuted,
            tick_number: Some(5),
            summary: "Action executed".into(),
            timestamp: Utc::now(),
            snapshot: None,
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("snapshot"));
        assert!(!obj.contains_key("tick_number") || obj["tick_number"].is_number());
    }

    #[test]
    fn live_event_without_tick_number_omits_field() {
        let event = LiveEvent {
            event_type: EventType::VesselStarted,
            tick_number: None,
            summary: "Vessel started".into(),
            timestamp: Utc::now(),
            snapshot: None,
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("tick_number"));
        assert!(!obj.contains_key("snapshot"));
    }

    // ── T-4: EventLedger::by_type is object-safe ──

    /// Verifies that `EventLedger` can be used as a trait object (`dyn EventLedger`).
    /// If this function compiles, the trait is object-safe including `by_type`.
    #[allow(dead_code)]
    fn assert_event_ledger_object_safe(l: &dyn EventLedger) {
        let _ = l.by_type(EventType::TickStarted, 1);
        let _ = l.recent(1);
        let _ = l.for_tick(TickId::new());
    }

    #[test]
    fn event_ledger_by_type_is_object_safe() {
        // The real test is that assert_event_ledger_object_safe compiles.
        // If EventLedger were not object-safe, `&dyn EventLedger` would be rejected.
        let _: fn(&dyn EventLedger) = assert_event_ledger_object_safe;
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

    // ── E4S4-T1: capability_request_event_serializes ──

    #[test]
    fn capability_request_event_serializes() {
        let json = serde_json::to_string(&EventType::CapabilityRequest).unwrap();
        assert_eq!(json, "\"capability_request\"");
        let parsed: EventType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, EventType::CapabilityRequest);
    }

    // ── E4S4-T2: capability_request_payload_roundtrip ──

    #[test]
    fn capability_request_payload_roundtrip() {
        let payload = CapabilityRequestPayload {
            capability: "discord".into(),
            reason: "trust gate: minimum trust (0.25) below threshold (0.60)".into(),
            context: "Need to send notification to #alerts channel".into(),
            acknowledged: false,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: CapabilityRequestPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, parsed);

        // Verify acknowledged defaults to false when omitted
        let without_ack = r#"{"capability":"test","reason":"r","context":"c"}"#;
        let parsed: CapabilityRequestPayload = serde_json::from_str(without_ack).unwrap();
        assert!(!parsed.acknowledged);
    }
}
