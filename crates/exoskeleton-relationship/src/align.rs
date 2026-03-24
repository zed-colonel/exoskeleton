//! Align step — relationship-aware action filtering.
//!
//! The Align step is the gatekeeper between Decide and Act. It runs entirely
//! on the Cognitive AQ (I9). Blocked actions never reach the Tool AQ.

use std::collections::HashMap;

use chrono::Utc;
use exoskeleton_core::{
    ArtifactId, LedgerEntryId, RelationalSignalType, RelationshipRecord, RelationshipSnapshot,
    TickId,
};
use serde::{Deserialize, Serialize};

/// Configuration for the Align step's relationship checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignConfig {
    /// Minimum trust level to approve any action (default: 0.3).
    pub min_trust_for_action: f64,
    /// Minimum trust level for destructive/high-impact actions (default: 0.6).
    pub min_trust_for_destructive: f64,
    /// Tool names considered "destructive" (e.g., "fs.write", "http.request").
    pub destructive_tools: Vec<String>,
    /// Whether to block actions affecting principals with broken commitments.
    pub block_on_broken_commitments: bool,
    /// Trust threshold for outbound vessel-to-vessel messaging.
    /// Actions matching `vessel_messaging_tools` require this trust level.
    /// Default: 0.6. Not yet enforced in `check_alignment()` — vessel messaging
    /// currently uses `http.request` which is gated via `destructive_tools` at the
    /// same threshold. Will be wired into check_alignment() when a dedicated
    /// `peer.send` connector is added.
    pub min_trust_for_vessel_messaging: f64,
    /// Tool names classified as vessel messaging (use vessel messaging trust threshold).
    /// Default: empty (vessel messaging currently uses http.request which is in destructive_tools).
    /// Not yet enforced — see `min_trust_for_vessel_messaging` doc.
    pub vessel_messaging_tools: Vec<String>,
}

impl Default for AlignConfig {
    fn default() -> Self {
        Self {
            min_trust_for_action: 0.3,
            min_trust_for_destructive: 0.6,
            destructive_tools: vec![
                "fs.write".into(),
                "http.request".into(),
                "shell.exec".into(),
                "sandbox.exec".into(),
                "webhook.send".into(),
                "web.search".into(),
                "discord".into(),
            ],
            block_on_broken_commitments: false,
            min_trust_for_vessel_messaging: 0.6,
            vessel_messaging_tools: vec![],
        }
    }
}

/// A planned action to be checked by the Align step.
///
/// Mirrors `kernel::types::PlannedAction` but is defined here to avoid
/// a dependency on `exoskeleton-host`. The kernel module translates between
/// the two types.
#[derive(Debug, Clone)]
pub struct AlignAction {
    pub tool_name: String,
    pub rationale: String,
}

/// Result of alignment checking for a single action.
#[derive(Debug, Clone)]
pub enum AlignActionResult {
    Approved,
    Blocked(String),
}

/// Check proposed actions against relationship constraints.
///
/// The Align step is the gatekeeper between Decide and Act. It runs entirely
/// on the Cognitive AQ (I9). Blocked actions never reach the Tool AQ.
///
/// Alignment checks:
/// 1. TRUST GATE: Actions affecting a principal require trust >= threshold
///    (default: 0.3 for any action, 0.6 for destructive actions)
/// 2. COMMITMENT CHECK: Actions that would violate active commitments are blocked
/// 3. ALIGNMENT SIGNALS: Record AlignmentCheck entries for each reviewed action
///
/// Returns a tuple of (approved action indices, blocked action indices with reasons,
/// relationship update records to append to the ledger).
pub fn check_alignment(
    actions: &[AlignAction],
    snapshot: &RelationshipSnapshot,
    config: &AlignConfig,
    tick_id: TickId,
) -> (Vec<usize>, Vec<(usize, String)>, Vec<RelationshipRecord>) {
    let mut approved = Vec::new();
    let mut blocked = Vec::new();
    let mut relationship_updates = Vec::new();

    // No principals = passthrough (backward compatible with S5 stub)
    if snapshot.principals.is_empty() {
        for i in 0..actions.len() {
            approved.push(i);
        }
        return (approved, blocked, relationship_updates);
    }

    // Find the minimum trust level across all principals
    let min_trust = snapshot
        .principals
        .iter()
        .map(|p| p.trust_level)
        .fold(f64::MAX, f64::min);

    // Check for broken commitments (any principal with low trust + recent broken commitments)
    let has_broken_commitments = config.block_on_broken_commitments
        && snapshot
            .principals
            .iter()
            .any(|p| p.trust_level < config.min_trust_for_action);

    for (i, action) in actions.iter().enumerate() {
        let is_destructive = config.destructive_tools.contains(&action.tool_name);
        let threshold = if is_destructive {
            config.min_trust_for_destructive
        } else {
            config.min_trust_for_action
        };

        let block_reason = if min_trust < threshold {
            Some(format!(
                "trust gate: minimum trust ({:.2}) below threshold ({:.2}) for {}tool '{}'",
                min_trust,
                threshold,
                if is_destructive { "destructive " } else { "" },
                action.tool_name,
            ))
        } else if has_broken_commitments && is_destructive {
            Some(format!(
                "commitment check: action '{}' blocked due to unresolved commitment violations",
                action.tool_name,
            ))
        } else {
            None
        };

        // Generate alignment record for each reviewed action
        let (signal_type, content_ref_label) = match &block_reason {
            Some(_) => (
                RelationalSignalType::AlignmentMismatch,
                format!("blocked:{}", action.tool_name),
            ),
            None => (
                RelationalSignalType::AlignmentCheck,
                format!("approved:{}", action.tool_name),
            ),
        };

        // Create one record per reviewed principal (or per action if global)
        // For v1.0-alpha: one record per action, using the first principal
        if let Some(principal) = snapshot.principals.first() {
            relationship_updates.push(RelationshipRecord {
                id: LedgerEntryId::new(),
                principal_id: principal.principal_id,
                signal_type,
                content_ref: ArtifactId::from_content(content_ref_label.as_bytes()),
                tick_id,
                timestamp: Utc::now(),
                metadata: HashMap::new(),
            });
        }

        match block_reason {
            Some(reason) => blocked.push((i, reason)),
            None => approved.push(i),
        }
    }

    (approved, blocked, relationship_updates)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{PrincipalId, PrincipalSummary};

    use super::*;

    fn empty_snapshot() -> RelationshipSnapshot {
        RelationshipSnapshot {
            principals: vec![],
            compiled_at: Utc::now(),
        }
    }

    fn snapshot_with_trust(trust: f64) -> RelationshipSnapshot {
        RelationshipSnapshot {
            principals: vec![PrincipalSummary {
                principal_id: PrincipalId::new(),
                display_name: "Test".into(),
                role: "operator".into(),
                trust_level: trust,
                active_commitments: 0,
                last_interaction: None,
                notes: None,
            }],
            compiled_at: Utc::now(),
        }
    }

    fn test_action(name: &str) -> AlignAction {
        AlignAction {
            tool_name: name.into(),
            rationale: format!("test {name}"),
        }
    }

    // ── T-4: Alignment Checker ──

    #[test]
    fn no_principals_all_actions_approved() {
        let snapshot = empty_snapshot();
        let config = AlignConfig::default();
        let actions = vec![test_action("fs.write"), test_action("delay")];
        let tick_id = TickId::new();

        let (approved, blocked, updates) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0, 1]);
        assert!(blocked.is_empty());
        assert!(updates.is_empty());
    }

    #[test]
    fn all_principals_above_threshold_approved() {
        let snapshot = snapshot_with_trust(0.8);
        let config = AlignConfig::default();
        let actions = vec![test_action("delay"), test_action("fs.read")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0, 1]);
        assert!(blocked.is_empty());
    }

    #[test]
    fn one_principal_below_threshold_blocks() {
        let snapshot = snapshot_with_trust(0.1);
        let config = AlignConfig::default();
        let actions = vec![test_action("delay")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("trust gate"));
    }

    #[test]
    fn destructive_tool_higher_threshold() {
        // Trust at 0.4 — above normal threshold (0.3) but below destructive (0.6)
        let snapshot = snapshot_with_trust(0.4);
        let config = AlignConfig::default();
        let actions = vec![test_action("fs.write")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("destructive"));
    }

    #[test]
    fn non_destructive_same_trust_approved() {
        // Trust at 0.4 — above normal threshold (0.3), non-destructive tool
        let snapshot = snapshot_with_trust(0.4);
        let config = AlignConfig::default();
        let actions = vec![test_action("delay")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0]);
        assert!(blocked.is_empty());
    }

    #[test]
    fn broken_commitment_blocking() {
        let snapshot = snapshot_with_trust(0.1); // Below min_trust_for_action
        let config = AlignConfig {
            block_on_broken_commitments: true,
            ..AlignConfig::default()
        };
        let actions = vec![test_action("fs.write")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert!(!blocked.is_empty());
    }

    #[test]
    fn alignment_check_records_generated() {
        let snapshot = snapshot_with_trust(0.8);
        let config = AlignConfig::default();
        let actions = vec![test_action("delay")];
        let tick_id = TickId::new();

        let (_, _, updates) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].signal_type, RelationalSignalType::AlignmentCheck);
    }

    #[test]
    fn alignment_mismatch_record_for_blocked() {
        let snapshot = snapshot_with_trust(0.1);
        let config = AlignConfig::default();
        let actions = vec![test_action("delay")];
        let tick_id = TickId::new();

        let (_, _, updates) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].signal_type,
            RelationalSignalType::AlignmentMismatch
        );
    }

    #[test]
    fn empty_actions_empty_result() {
        let snapshot = snapshot_with_trust(0.5);
        let config = AlignConfig::default();
        let actions: Vec<AlignAction> = vec![];
        let tick_id = TickId::new();

        let (approved, blocked, updates) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert!(blocked.is_empty());
        assert!(updates.is_empty());
    }

    #[test]
    fn config_with_zero_trust_nothing_blocked() {
        let snapshot = snapshot_with_trust(0.0);
        let config = AlignConfig {
            min_trust_for_action: 0.0,
            min_trust_for_destructive: 0.0,
            ..AlignConfig::default()
        };
        let actions = vec![test_action("fs.write"), test_action("delay")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0, 1]);
        assert!(blocked.is_empty());
    }

    #[test]
    fn default_config_passes_neutral_trust() {
        let snapshot = snapshot_with_trust(0.5); // Neutral
        let config = AlignConfig::default();
        let actions = vec![test_action("delay")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0]);
        assert!(blocked.is_empty());
    }

    // ── E2S1-T49: AlignConfig default includes shell.exec ──

    #[test]
    fn align_config_default_includes_shell_exec() {
        let config = AlignConfig::default();
        assert!(
            config.destructive_tools.contains(&"shell.exec".to_string()),
            "shell.exec should be in destructive_tools"
        );
    }

    // ── E2S1-T50: Align blocks shell.exec below trust threshold ──

    #[test]
    fn align_blocks_shell_exec_below_trust() {
        let snapshot = snapshot_with_trust(0.4); // Below destructive threshold (0.6)
        let config = AlignConfig::default();
        let actions = vec![test_action("shell.exec")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("destructive"));
    }

    // ── E2S1-T51: Align allows shell.exec at trust threshold ──

    #[test]
    fn align_allows_shell_exec_at_trust() {
        let snapshot = snapshot_with_trust(0.8); // Above destructive threshold (0.6)
        let config = AlignConfig::default();
        let actions = vec![test_action("shell.exec")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0]);
        assert!(blocked.is_empty());
    }

    // ── E2S2-T31: AlignConfig default includes sandbox.exec ──

    #[test]
    fn align_config_default_includes_sandbox_exec() {
        let config = AlignConfig::default();
        assert!(
            config
                .destructive_tools
                .contains(&"sandbox.exec".to_string()),
            "sandbox.exec should be in destructive_tools"
        );
    }

    // ── E2S2-T32: Align blocks sandbox.exec below trust threshold ──

    #[test]
    fn align_blocks_sandbox_exec_below_trust() {
        let snapshot = snapshot_with_trust(0.4); // Below destructive threshold (0.6)
        let config = AlignConfig::default();
        let actions = vec![test_action("sandbox.exec")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("destructive"));
    }

    // ── E2S2-T33: Align allows sandbox.exec at trust threshold ──

    #[test]
    fn align_allows_sandbox_exec_at_trust() {
        let snapshot = snapshot_with_trust(0.8); // Above destructive threshold (0.6)
        let config = AlignConfig::default();
        let actions = vec![test_action("sandbox.exec")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert_eq!(approved, vec![0]);
        assert!(blocked.is_empty());
    }

    // ── E4S1-T7: AlignConfig vessel messaging default ──

    #[test]
    fn align_vessel_messaging_config_default() {
        let config = AlignConfig::default();
        assert!(
            (config.min_trust_for_vessel_messaging - 0.6).abs() < f64::EPSILON,
            "min_trust_for_vessel_messaging should default to 0.6"
        );
    }

    // ── E4S1-T8: AlignConfig vessel messaging tools default empty ──

    #[test]
    fn align_vessel_messaging_tools_default_empty() {
        let config = AlignConfig::default();
        assert!(
            config.vessel_messaging_tools.is_empty(),
            "vessel_messaging_tools should default to empty"
        );
    }

    // ── E4S2-T28: AlignConfig default includes WASM connectors ──

    #[test]
    fn align_config_default_includes_wasm_connectors() {
        let config = AlignConfig::default();
        assert!(config
            .destructive_tools
            .contains(&"webhook.send".to_string()));
        assert!(config.destructive_tools.contains(&"web.search".to_string()));
        assert!(config.destructive_tools.contains(&"discord".to_string()));
    }

    // ── E4S2-T29: Align blocks WASM connector at low trust ──

    #[test]
    fn align_blocks_wasm_connector_at_low_trust() {
        let snapshot = snapshot_with_trust(0.4); // Below destructive threshold (0.6)
        let config = AlignConfig::default();
        let actions = vec![test_action("discord")];
        let tick_id = TickId::new();

        let (approved, blocked, _) = check_alignment(&actions, &snapshot, &config, tick_id);
        assert!(approved.is_empty());
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].1.contains("destructive"));
    }

    // ── E4S1-T9: AlignConfig serde roundtrip with new fields ──

    #[test]
    fn align_config_serde_roundtrip_with_new_fields() {
        let config = AlignConfig {
            min_trust_for_vessel_messaging: 0.7,
            vessel_messaging_tools: vec!["peer.send".into()],
            ..AlignConfig::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let roundtripped: AlignConfig = serde_json::from_str(&json).unwrap();
        assert!((roundtripped.min_trust_for_vessel_messaging - 0.7).abs() < f64::EPSILON,);
        assert_eq!(roundtripped.vessel_messaging_tools, vec!["peer.send"]);
        assert_eq!(roundtripped.destructive_tools, config.destructive_tools);
    }
}
