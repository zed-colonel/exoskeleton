//! Relationship Snapshot compiler — trust computation from the ledger.
//!
//! Pure function that compiles a fresh `RelationshipSnapshot` from the full
//! ledger. Called every tick in the Align step (I5: compiled, not accumulated).

use chrono::{DateTime, Utc};
use exoskeleton_core::{
    ExoError, PrincipalSummary, RelationalSignalType, RelationshipSnapshot, TrustDecayConfig,
};

use crate::ledger::RelationshipLedger;

/// Compile a fresh RelationshipSnapshot from the Relationship Ledger.
///
/// This function reads the full ledger and produces a compact summary.
/// Called every tick in the Align step (I5: compiled, not accumulated).
///
/// Trust computation for each principal:
/// - Starts at neutral (0.5)
/// - `TrustUpdate` signals set the trust level directly (most recent wins)
/// - `CommitmentFulfilled` → +0.05 (capped at 1.0)
/// - `CommitmentBroken` → -0.15 (floored at 0.0)
/// - `FeedbackReceived` → ±0.02 (positive by default)
/// - `AlignmentMismatch` → -0.05
/// - Decay: exponential time-based decay toward baseline (E1-S3, W-15)
///
/// Active commitments: count of `CommitmentMade` signals minus
/// `CommitmentFulfilled` and `CommitmentBroken` signals per principal.
///
/// Last interaction: timestamp of the most recent record per principal.
pub fn compile_relationship_snapshot(
    ledger: &dyn RelationshipLedger,
    decay_config: Option<&TrustDecayConfig>,
    now: DateTime<Utc>,
) -> Result<RelationshipSnapshot, ExoError> {
    let principal_ids = ledger.distinct_principals()?;

    let mut principals = Vec::with_capacity(principal_ids.len());

    for principal_id in principal_ids {
        // Get full history for this principal (newest first)
        let records = ledger.for_principal(principal_id, usize::MAX)?;

        if records.is_empty() {
            continue;
        }

        // Walk records chronologically (reverse the newest-first order)
        let mut trust_level: f64 = 0.5;
        let mut active_commitments: i64 = 0;
        let mut display_name = principal_id.to_string();
        let mut role = "unknown".to_string();
        let mut notes_parts: Vec<String> = Vec::new();

        // Last interaction is the most recent record's timestamp
        let last_interaction = records.first().map(|r| r.timestamp);

        // Process chronologically (oldest first)
        for record in records.iter().rev() {
            match record.signal_type {
                RelationalSignalType::TrustUpdate => {
                    // TrustUpdate sets trust level directly. If the record's
                    // metadata contains "trust_level", use that value. Otherwise
                    // fall back to neutral (0.5). The metadata is populated by
                    // process_relational_signals() from the RelationalSignal's
                    // metadata, so the snapshot compiler needs no artifact store.
                    trust_level = record
                        .metadata
                        .get("trust_level")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.5)
                        .clamp(0.0, 1.0);
                }
                RelationalSignalType::CommitmentMade => {
                    active_commitments += 1;
                }
                RelationalSignalType::CommitmentFulfilled => {
                    trust_level = (trust_level + 0.05).min(1.0);
                    active_commitments = (active_commitments - 1).max(0);
                }
                RelationalSignalType::CommitmentBroken => {
                    trust_level = (trust_level - 0.15).max(0.0);
                    active_commitments = (active_commitments - 1).max(0);
                }
                RelationalSignalType::FeedbackReceived => {
                    // Default: positive feedback (+0.02). If metadata contains
                    // "sentiment" = "negative", apply -0.02 instead.
                    let delta = if record.metadata.get("sentiment").map(|s| s.as_str())
                        == Some("negative")
                    {
                        -0.02
                    } else {
                        0.02
                    };
                    trust_level = (trust_level + delta).clamp(0.0, 1.0);
                }
                RelationalSignalType::AlignmentMismatch => {
                    trust_level = (trust_level - 0.05).max(0.0);
                }
                RelationalSignalType::AlignmentCheck => {
                    // No trust impact — audit trail only
                }
                RelationalSignalType::ToneObservation => {
                    // No trust impact — qualitative notes only
                }
            }
        }

        // Derive display_name and role from earliest signal's metadata.
        // Walk chronologically (already reversed above), take first match.
        for record in records.iter().rev() {
            if let Some(name) = record.metadata.get("display_name") {
                display_name = name.clone();
                break;
            }
        }
        for record in records.iter().rev() {
            if let Some(r) = record.metadata.get("role") {
                role = r.clone();
                break;
            }
        }

        // Derive notes from recent FeedbackReceived and ToneObservation
        let note_records: Vec<_> = records
            .iter()
            .filter(|r| {
                matches!(
                    r.signal_type,
                    RelationalSignalType::FeedbackReceived | RelationalSignalType::ToneObservation
                )
            })
            .take(3)
            .collect();

        for nr in note_records {
            notes_parts.push(format!("{:?}", nr.signal_type));
        }

        // Apply time-based trust decay (E1-S3, W-15)
        if let Some(config) = decay_config {
            if let Some(last) = last_interaction {
                let elapsed_days = (now - last).num_days();
                if elapsed_days >= config.min_inactivity_days as i64 {
                    let decay_factor = 1.0 - (-config.decay_rate * elapsed_days as f64).exp();
                    trust_level = trust_level + decay_factor * (config.baseline - trust_level);
                    trust_level = trust_level.clamp(0.0, 1.0);
                }
            }
        }

        let notes = if notes_parts.is_empty() {
            None
        } else {
            Some(notes_parts.join(", "))
        };

        principals.push(PrincipalSummary {
            principal_id,
            display_name,
            role,
            trust_level,
            active_commitments: active_commitments.max(0) as u32,
            last_interaction,
            notes,
        });
    }

    Ok(RelationshipSnapshot {
        principals,
        compiled_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{ArtifactId, LedgerEntryId, PrincipalId, RelationshipRecord, TickId};

    use super::*;
    use crate::ledger::InMemoryRelationshipLedger;

    fn make_record(
        principal_id: PrincipalId,
        signal_type: RelationalSignalType,
        ts_offset_secs: i64,
    ) -> RelationshipRecord {
        RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id,
            signal_type,
            content_ref: ArtifactId::from_content(b"test"),
            tick_id: TickId::new(),
            timestamp: Utc::now() + chrono::Duration::seconds(ts_offset_secs),
            metadata: Default::default(),
        }
    }

    // ── T-2: Snapshot Compiler ──

    #[test]
    fn empty_ledger_produces_empty_snapshot() {
        let ledger = InMemoryRelationshipLedger::new();
        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert!(snapshot.principals.is_empty());
    }

    #[test]
    fn single_principal_with_trust_update() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        ledger
            .append(&make_record(p, RelationalSignalType::TrustUpdate, 0))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert_eq!(snapshot.principals.len(), 1);
        // TrustUpdate resets to 0.5 (neutral)
        assert!((snapshot.principals[0].trust_level - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn multiple_signals_accumulate_trust() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        // Start at 0.5 (neutral), then +0.05 (fulfilled), +0.02 (feedback)
        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade, 0))
            .unwrap();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentFulfilled,
                1,
            ))
            .unwrap();
        ledger
            .append(&make_record(p, RelationalSignalType::FeedbackReceived, 2))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert_eq!(snapshot.principals.len(), 1);
        let expected = 0.5 + 0.05 + 0.02;
        assert!(
            (snapshot.principals[0].trust_level - expected).abs() < f64::EPSILON,
            "expected {expected}, got {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn commitment_made_fulfilled_broken_tracked() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade, 0))
            .unwrap();
        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade, 1))
            .unwrap();
        // active = 2
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentFulfilled,
                2,
            ))
            .unwrap();
        // active = 1

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert_eq!(snapshot.principals[0].active_commitments, 1);
    }

    #[test]
    fn commitment_broken_decreases_trust() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade, 0))
            .unwrap();
        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentBroken, 1))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        let expected = 0.5 - 0.15;
        assert!(
            (snapshot.principals[0].trust_level - expected).abs() < f64::EPSILON,
            "expected {expected}, got {}",
            snapshot.principals[0].trust_level
        );
        assert_eq!(snapshot.principals[0].active_commitments, 0);
    }

    #[test]
    fn commitment_fulfilled_increases_trust() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade, 0))
            .unwrap();
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentFulfilled,
                1,
            ))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        let expected = 0.5 + 0.05;
        assert!(
            (snapshot.principals[0].trust_level - expected).abs() < f64::EPSILON,
            "expected {expected}, got {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn trust_clamped_to_zero_one() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        // Many broken commitments should floor at 0.0
        for i in 0..10 {
            ledger
                .append(&make_record(p, RelationalSignalType::CommitmentMade, i))
                .unwrap();
            ledger
                .append(&make_record(
                    p,
                    RelationalSignalType::CommitmentBroken,
                    i + 1,
                ))
                .unwrap();
        }

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert!(snapshot.principals[0].trust_level >= 0.0);
        assert!(snapshot.principals[0].trust_level <= 1.0);
    }

    #[test]
    fn active_commitments_floored_at_zero() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        // Fulfill without making — should not go negative
        ledger
            .append(&make_record(
                p,
                RelationalSignalType::CommitmentFulfilled,
                0,
            ))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert_eq!(snapshot.principals[0].active_commitments, 0);
    }

    #[test]
    fn last_interaction_reflects_most_recent_record() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        let r1 = make_record(p, RelationalSignalType::TrustUpdate, 0);
        ledger.append(&r1).unwrap();

        let r2 = make_record(p, RelationalSignalType::FeedbackReceived, 10);
        let t2 = r2.timestamp;
        ledger.append(&r2).unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        let last = snapshot.principals[0].last_interaction.unwrap();
        assert_eq!(last, t2);
    }

    #[test]
    fn multiple_principals_compiled_independently() {
        let ledger = InMemoryRelationshipLedger::new();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        // p1: neutral (0.5) + fulfilled (+0.05) = 0.55
        ledger
            .append(&make_record(p1, RelationalSignalType::CommitmentMade, 0))
            .unwrap();
        ledger
            .append(&make_record(
                p1,
                RelationalSignalType::CommitmentFulfilled,
                1,
            ))
            .unwrap();

        // p2: neutral (0.5) + broken (-0.15) = 0.35
        ledger
            .append(&make_record(p2, RelationalSignalType::CommitmentMade, 2))
            .unwrap();
        ledger
            .append(&make_record(p2, RelationalSignalType::CommitmentBroken, 3))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert_eq!(snapshot.principals.len(), 2);

        let s1 = snapshot
            .principals
            .iter()
            .find(|s| s.principal_id == p1)
            .unwrap();
        let s2 = snapshot
            .principals
            .iter()
            .find(|s| s.principal_id == p2)
            .unwrap();

        assert!((s1.trust_level - 0.55).abs() < f64::EPSILON);
        assert!((s2.trust_level - 0.35).abs() < f64::EPSILON);
    }

    #[test]
    fn notes_derived_from_feedback_and_tone() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        ledger
            .append(&make_record(p, RelationalSignalType::FeedbackReceived, 0))
            .unwrap();
        ledger
            .append(&make_record(p, RelationalSignalType::ToneObservation, 1))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert!(snapshot.principals[0].notes.is_some());
        let notes = snapshot.principals[0].notes.as_ref().unwrap();
        assert!(notes.contains("FeedbackReceived"));
        assert!(notes.contains("ToneObservation"));
    }

    #[test]
    fn trust_update_with_explicit_trust_level() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        let mut record = make_record(p, RelationalSignalType::TrustUpdate, 0);
        record.metadata.insert("trust_level".into(), "0.85".into());
        ledger.append(&record).unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert!(
            (snapshot.principals[0].trust_level - 0.85).abs() < f64::EPSILON,
            "expected 0.85, got {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn trust_update_without_metadata_falls_back_to_neutral() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        // TrustUpdate with no trust_level in metadata → neutral 0.5
        ledger
            .append(&make_record(p, RelationalSignalType::TrustUpdate, 0))
            .unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert!(
            (snapshot.principals[0].trust_level - 0.5).abs() < f64::EPSILON,
            "expected 0.5, got {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn display_name_and_role_from_metadata() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        let mut record = make_record(p, RelationalSignalType::TrustUpdate, 0);
        record
            .metadata
            .insert("display_name".into(), "Alice".into());
        record.metadata.insert("role".into(), "operator".into());
        ledger.append(&record).unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        assert_eq!(snapshot.principals[0].display_name, "Alice");
        assert_eq!(snapshot.principals[0].role, "operator");
    }

    #[test]
    fn negative_feedback_decreases_trust() {
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();

        let mut record = make_record(p, RelationalSignalType::FeedbackReceived, 0);
        record
            .metadata
            .insert("sentiment".into(), "negative".into());
        ledger.append(&record).unwrap();

        let snapshot = compile_relationship_snapshot(&ledger, None, Utc::now()).unwrap();
        let expected = 0.5 - 0.02;
        assert!(
            (snapshot.principals[0].trust_level - expected).abs() < f64::EPSILON,
            "expected {expected}, got {}",
            snapshot.principals[0].trust_level
        );
    }

    // ── E1-T67–E1-T74: Trust Decay Tests ──

    fn make_record_at(
        principal_id: PrincipalId,
        signal_type: RelationalSignalType,
        timestamp: chrono::DateTime<Utc>,
    ) -> exoskeleton_core::RelationshipRecord {
        exoskeleton_core::RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id,
            signal_type,
            content_ref: ArtifactId::from_content(b"test"),
            tick_id: TickId::new(),
            timestamp,
            metadata: Default::default(),
        }
    }

    #[test]
    fn trust_decays_toward_baseline_after_inactivity() {
        // E1-T67: 30 days inactive, rate 0.01 → ~26% decay toward 0.5
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let thirty_days_ago = now - chrono::Duration::days(30);

        // Build trust to 0.8 via explicit TrustUpdate
        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, thirty_days_ago);
        record.metadata.insert("trust_level".into(), "0.8".into());
        ledger.append(&record).unwrap();

        let config = exoskeleton_core::TrustDecayConfig::default();
        let snapshot = compile_relationship_snapshot(&ledger, Some(&config), now).unwrap();

        // Expected: 0.8 + (1 - exp(-0.01 * 30)) * (0.5 - 0.8)
        let decay_factor = 1.0 - (-0.01_f64 * 30.0).exp();
        let expected = 0.8 + decay_factor * (0.5 - 0.8);
        assert!(
            (snapshot.principals[0].trust_level - expected).abs() < 0.001,
            "expected ~{expected:.4}, got {:.4}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn recently_active_principal_trust_unchanged() {
        // E1-T68: Last interaction today → no decay
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, now);
        record.metadata.insert("trust_level".into(), "0.9".into());
        ledger.append(&record).unwrap();

        let config = exoskeleton_core::TrustDecayConfig::default();
        let snapshot = compile_relationship_snapshot(&ledger, Some(&config), now).unwrap();

        assert!(
            (snapshot.principals[0].trust_level - 0.9).abs() < f64::EPSILON,
            "trust should be unchanged: {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn high_trust_decays_downward() {
        // E1-T69: Trust above 0.5 decays toward 0.5
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let long_ago = now - chrono::Duration::days(70);

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, long_ago);
        record.metadata.insert("trust_level".into(), "0.9".into());
        ledger.append(&record).unwrap();

        let config = exoskeleton_core::TrustDecayConfig::default();
        let snapshot = compile_relationship_snapshot(&ledger, Some(&config), now).unwrap();

        assert!(
            snapshot.principals[0].trust_level < 0.9,
            "trust should decay below 0.9: {}",
            snapshot.principals[0].trust_level
        );
        assert!(
            snapshot.principals[0].trust_level > 0.5,
            "trust should still be above baseline: {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn low_trust_decays_upward_rehabilitation() {
        // E1-T70: Trust below 0.5 decays upward toward 0.5
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let long_ago = now - chrono::Duration::days(70);

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, long_ago);
        record.metadata.insert("trust_level".into(), "0.1".into());
        ledger.append(&record).unwrap();

        let config = exoskeleton_core::TrustDecayConfig::default();
        let snapshot = compile_relationship_snapshot(&ledger, Some(&config), now).unwrap();

        assert!(
            snapshot.principals[0].trust_level > 0.1,
            "trust should rehabilitate above 0.1: {}",
            snapshot.principals[0].trust_level
        );
        assert!(
            snapshot.principals[0].trust_level < 0.5,
            "trust should still be below baseline: {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn min_inactivity_threshold_respected() {
        // E1-T71: 3 days inactive, 7-day threshold → no decay
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let three_days_ago = now - chrono::Duration::days(3);

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, three_days_ago);
        record.metadata.insert("trust_level".into(), "0.8".into());
        ledger.append(&record).unwrap();

        let config = exoskeleton_core::TrustDecayConfig::default(); // min_inactivity_days = 7
        let snapshot = compile_relationship_snapshot(&ledger, Some(&config), now).unwrap();

        assert!(
            (snapshot.principals[0].trust_level - 0.8).abs() < f64::EPSILON,
            "trust should be unchanged within threshold: {}",
            snapshot.principals[0].trust_level
        );
    }

    #[test]
    fn custom_decay_rate_produces_different_amount() {
        // E1-T72: Higher rate → more decay
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let thirty_days_ago = now - chrono::Duration::days(30);

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, thirty_days_ago);
        record.metadata.insert("trust_level".into(), "0.8".into());
        ledger.append(&record).unwrap();

        let slow_config = exoskeleton_core::TrustDecayConfig {
            decay_rate: 0.005,
            ..Default::default()
        };
        let fast_config = exoskeleton_core::TrustDecayConfig {
            decay_rate: 0.05,
            ..Default::default()
        };

        let slow = compile_relationship_snapshot(&ledger, Some(&slow_config), now).unwrap();
        let fast = compile_relationship_snapshot(&ledger, Some(&fast_config), now).unwrap();

        // Faster decay should produce lower trust (closer to 0.5 baseline)
        assert!(
            fast.principals[0].trust_level < slow.principals[0].trust_level,
            "fast decay ({}) should be lower than slow ({})",
            fast.principals[0].trust_level,
            slow.principals[0].trust_level
        );
    }

    #[test]
    fn trust_stays_clamped_after_decay() {
        // E1-T73: Trust stays within [0.0, 1.0]
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let very_long_ago = now - chrono::Duration::days(10000);

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, very_long_ago);
        record.metadata.insert("trust_level".into(), "0.99".into());
        ledger.append(&record).unwrap();

        let config = exoskeleton_core::TrustDecayConfig::default();
        let snapshot = compile_relationship_snapshot(&ledger, Some(&config), now).unwrap();

        assert!(snapshot.principals[0].trust_level >= 0.0);
        assert!(snapshot.principals[0].trust_level <= 1.0);
    }

    #[test]
    fn none_decay_config_preserves_existing_behavior() {
        // E1-T74: None decay config → identical to pre-E1-S3
        let ledger = InMemoryRelationshipLedger::new();
        let p = PrincipalId::new();
        let now = Utc::now();
        let long_ago = now - chrono::Duration::days(365);

        let mut record = make_record_at(p, RelationalSignalType::TrustUpdate, long_ago);
        record.metadata.insert("trust_level".into(), "0.8".into());
        ledger.append(&record).unwrap();

        let with_none = compile_relationship_snapshot(&ledger, None, now).unwrap();

        // Without decay config, trust should remain exactly at signal value
        assert!(
            (with_none.principals[0].trust_level - 0.8).abs() < f64::EPSILON,
            "trust should be unchanged with None config: {}",
            with_none.principals[0].trust_level
        );
    }
}
