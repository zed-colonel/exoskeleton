//! Relationship domain primitives — Ledger, Snapshot, and signal types.
//!
//! The Relationship Substrate (I8) maintains explicit, durable models of
//! trust, commitments, and alignment with every principal (human or agent).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, LedgerEntryId, PrincipalId, TickId};

/// One entry in the Relationship Ledger (I8: durable and explicit).
///
/// The ledger is append-only — entries are never modified or deleted (IBP §4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct RelationshipRecord {
    /// Unique identity of this ledger entry.
    pub id: LedgerEntryId,
    /// Which principal this entry concerns.
    pub principal_id: PrincipalId,
    /// What type of relational signal produced this entry.
    pub signal_type: RelationalSignalType,
    /// Reference to the artifact containing the full signal content.
    pub content_ref: ArtifactId,
    /// Which tick produced this entry (Cognitive AQ tick).
    pub tick_id: TickId,
    /// When this entry was created.
    pub timestamp: DateTime<Utc>,
    /// Signal-specific metadata carried on the record itself.
    ///
    /// This allows the snapshot compiler (a pure function with no artifact store
    /// access) to read signal parameters directly. For example, a `TrustUpdate`
    /// signal stores `{"trust_level": "0.8"}` here so the compiler can set
    /// trust precisely rather than defaulting to neutral.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub metadata: std::collections::HashMap<String, String>,
}

/// Classification of relational signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RelationalSignalType {
    /// Trust level changed for a principal.
    TrustUpdate,
    /// A commitment was made to a principal.
    CommitmentMade,
    /// A commitment to a principal was fulfilled.
    CommitmentFulfilled,
    /// A commitment to a principal was broken.
    CommitmentBroken,
    /// Feedback was received from a principal.
    FeedbackReceived,
    /// An alignment check was performed.
    AlignmentCheck,
    /// An alignment mismatch was detected.
    AlignmentMismatch,
    /// A tone or affect observation was recorded.
    ToneObservation,
}

/// Compact always-in-context summary of all relationships (I5: compiled, not accumulated).
///
/// Recomputed every tick in the Align step from the Relationship Ledger.
/// Never accumulated — compiled fresh from durable sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct RelationshipSnapshot {
    /// Summary of each known principal.
    pub principals: Vec<PrincipalSummary>,
    /// When this snapshot was compiled.
    pub compiled_at: DateTime<Utc>,
}

/// Compact summary of the vessel's relationship with one principal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct PrincipalSummary {
    /// Which principal this summarizes.
    pub principal_id: PrincipalId,
    /// Human-readable name.
    pub display_name: String,
    /// Role of the principal (e.g., "operator", "peer-agent", "auditor").
    pub role: String,
    /// Trust level from 0.0 (no trust) to 1.0 (full trust). Neutral = 0.5.
    pub trust_level: f64,
    /// Number of active commitments to this principal.
    pub active_commitments: u32,
    /// When we last interacted with this principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_interaction: Option<DateTime<Utc>>,
    /// Free-form notes about this relationship.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Configuration for time-based trust decay (E1-S3, W-15).
///
/// After signal-based trust computation, trust decays exponentially toward
/// the baseline for inactive principals. Active principals (recent interaction)
/// are unaffected.
///
/// Formula:
///   elapsed = (now - last_interaction).num_days()
///   if elapsed >= min_inactivity_days:
///     decay_factor = 1.0 - exp(-decay_rate * elapsed as f64)
///     trust = trust + decay_factor * (baseline - trust)
///
/// With default parameters (rate=0.01, baseline=0.5, min_inactivity=7):
/// - 7 days inactive: ~6.8% decay toward 0.5
/// - 30 days inactive: ~26% decay toward 0.5
/// - 70 days inactive: ~50% decay toward 0.5
/// - Active principals: zero decay
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct TrustDecayConfig {
    /// Decay rate per day. Default: 0.01 (~50% decay in 70 days).
    pub decay_rate: f64,
    /// Trust level that inactive principals regress toward. Default: 0.5.
    pub baseline: f64,
    /// Minimum days of inactivity before decay begins. Default: 7.
    /// Prevents penalizing weekend gaps or brief absences.
    pub min_inactivity_days: u64,
}

impl Default for TrustDecayConfig {
    fn default() -> Self {
        Self {
            decay_rate: 0.01,
            baseline: 0.5,
            min_inactivity_days: 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-7: Relationship Primitives ──

    #[test]
    fn relationship_record_roundtrip() {
        let record = RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id: PrincipalId::new(),
            signal_type: RelationalSignalType::TrustUpdate,
            content_ref: ArtifactId::from_content(b"trust signal"),
            tick_id: TickId::new(),
            timestamp: Utc::now(),
            metadata: Default::default(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: RelationshipRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, parsed);
    }

    #[test]
    fn relational_signal_type_all_variants_roundtrip() {
        let variants = [
            RelationalSignalType::TrustUpdate,
            RelationalSignalType::CommitmentMade,
            RelationalSignalType::CommitmentFulfilled,
            RelationalSignalType::CommitmentBroken,
            RelationalSignalType::FeedbackReceived,
            RelationalSignalType::AlignmentCheck,
            RelationalSignalType::AlignmentMismatch,
            RelationalSignalType::ToneObservation,
        ];
        for st in &variants {
            let json = serde_json::to_string(st).unwrap();
            let parsed: RelationalSignalType = serde_json::from_str(&json).unwrap();
            assert_eq!(*st, parsed);
        }
    }

    #[test]
    fn relationship_snapshot_roundtrip() {
        let snap = RelationshipSnapshot {
            principals: vec![
                PrincipalSummary {
                    principal_id: PrincipalId::new(),
                    display_name: "Keith".into(),
                    role: "operator".into(),
                    trust_level: 0.95,
                    active_commitments: 3,
                    last_interaction: Some(Utc::now()),
                    notes: Some("Primary operator".into()),
                },
                PrincipalSummary {
                    principal_id: PrincipalId::new(),
                    display_name: "Auditor Bot".into(),
                    role: "auditor".into(),
                    trust_level: 0.5,
                    active_commitments: 0,
                    last_interaction: None,
                    notes: None,
                },
            ],
            compiled_at: Utc::now(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: RelationshipSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, parsed);
    }

    #[test]
    fn principal_summary_roundtrip_full() {
        let ps = PrincipalSummary {
            principal_id: PrincipalId::new(),
            display_name: "Agent X".into(),
            role: "peer-agent".into(),
            trust_level: 0.75,
            active_commitments: 2,
            last_interaction: Some(Utc::now()),
            notes: Some("Collaborative partner".into()),
        };
        let json = serde_json::to_string(&ps).unwrap();
        let parsed: PrincipalSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(ps, parsed);
    }

    #[test]
    fn principal_summary_roundtrip_minimal() {
        let ps = PrincipalSummary {
            principal_id: PrincipalId::new(),
            display_name: "Observer".into(),
            role: "monitor".into(),
            trust_level: 0.5,
            active_commitments: 0,
            last_interaction: None,
            notes: None,
        };
        let json = serde_json::to_string(&ps).unwrap();
        let parsed: PrincipalSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(ps, parsed);

        // Optional fields should be omitted
        let value: serde_json::Value = serde_json::to_value(&ps).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("last_interaction"));
        assert!(!obj.contains_key("notes"));
    }

    // ── E1-T75, E1-T76: TrustDecayConfig ──

    #[test]
    fn trust_decay_config_json_roundtrip() {
        let config = TrustDecayConfig {
            decay_rate: 0.02,
            baseline: 0.4,
            min_inactivity_days: 14,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: TrustDecayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn trust_decay_config_default_values() {
        let config = TrustDecayConfig::default();
        assert!((config.decay_rate - 0.01).abs() < f64::EPSILON);
        assert!((config.baseline - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.min_inactivity_days, 7);
    }

    #[test]
    fn principal_summary_trust_level_precision() {
        let ps = PrincipalSummary {
            principal_id: PrincipalId::new(),
            display_name: "Precision Test".into(),
            role: "test".into(),
            trust_level: 0.8732,
            active_commitments: 0,
            last_interaction: None,
            notes: None,
        };
        let json = serde_json::to_string(&ps).unwrap();
        let parsed: PrincipalSummary = serde_json::from_str(&json).unwrap();
        assert!(
            (parsed.trust_level - 0.8732).abs() < f64::EPSILON,
            "Trust level precision lost: {}",
            parsed.trust_level
        );
    }
}
