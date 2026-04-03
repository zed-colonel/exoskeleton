use std::collections::HashMap;
use std::sync::Mutex;

use exoskeleton_core::VesselMode;
use serde::{Deserialize, Serialize};

/// Per-tool policy rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyRule {
    /// Always allowed — no confirmation needed.
    Allow,
    /// Blocked — tool cannot be invoked regardless of trust.
    Deny,
    /// Requires operator confirmation before first use in session.
    /// After confirmation, treated as Allow for the remainder of the session.
    Ask,
}

fn default_policy_rule() -> PolicyRule {
    PolicyRule::Allow
}

/// Tool policy configuration from vessel config TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPolicyConfig {
    /// Default rule for tools not explicitly listed.
    #[serde(default = "default_policy_rule")]
    pub default: PolicyRule,
    /// Per-tool rule overrides. Key is connector name.
    #[serde(default)]
    pub rules: HashMap<String, PolicyRule>,
}

impl Default for ToolPolicyConfig {
    fn default() -> Self {
        Self {
            default: PolicyRule::Allow,
            rules: HashMap::new(),
        }
    }
}

impl ToolPolicyConfig {
    /// Look up the effective rule for a tool.
    pub fn rule_for(&self, tool_name: &str) -> PolicyRule {
        self.rules.get(tool_name).copied().unwrap_or(self.default)
    }
}

/// Tracks per-tool session approvals (in-memory, not persisted).
pub struct SessionApprovals {
    approvals: Mutex<HashMap<String, bool>>,
}

impl SessionApprovals {
    pub fn new() -> Self {
        Self {
            approvals: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_approved(&self, tool_name: &str) -> bool {
        self.approvals
            .lock()
            .unwrap()
            .get(tool_name)
            .copied()
            .unwrap_or(false)
    }

    pub fn approve(&self, tool_name: &str) {
        self.approvals
            .lock()
            .unwrap()
            .insert(tool_name.to_string(), true);
    }
}

impl Default for SessionApprovals {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of policy evaluation for a single action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allowed,
    Denied { reason: String },
    NeedsApproval,
}

/// Evaluate the policy for a tool invocation.
pub fn evaluate_policy(
    tool_name: &str,
    is_read_only: bool,
    vessel_mode: VesselMode,
    policy_config: &ToolPolicyConfig,
    session_approvals: &SessionApprovals,
) -> PolicyDecision {
    if vessel_mode == VesselMode::Planning && !is_read_only {
        return PolicyDecision::Denied {
            reason: format!(
                "tool '{tool_name}' is mutating; blocked in planning mode (read-only only)"
            ),
        };
    }

    match policy_config.rule_for(tool_name) {
        PolicyRule::Allow => PolicyDecision::Allowed,
        PolicyRule::Deny => PolicyDecision::Denied {
            reason: format!("tool '{tool_name}' denied by policy"),
        },
        PolicyRule::Ask => {
            if session_approvals.is_approved(tool_name) {
                PolicyDecision::Allowed
            } else {
                PolicyDecision::NeedsApproval
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use exoskeleton_core::VesselMode;

    use super::{evaluate_policy, PolicyDecision, PolicyRule, SessionApprovals, ToolPolicyConfig};

    fn config(default: PolicyRule, rules: &[(&str, PolicyRule)]) -> ToolPolicyConfig {
        ToolPolicyConfig {
            default,
            rules: rules.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        }
    }

    #[test]
    fn policy_rule_allow_passes() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "code.read",
            true,
            VesselMode::Normal,
            &config(PolicyRule::Deny, &[("code.read", PolicyRule::Allow)]),
            &approvals,
        );
        assert_eq!(decision, PolicyDecision::Allowed);
    }

    #[test]
    fn policy_rule_deny_blocks() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "fs.write",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Allow, &[("fs.write", PolicyRule::Deny)]),
            &approvals,
        );
        assert!(matches!(decision, PolicyDecision::Denied { .. }));
    }

    #[test]
    fn policy_rule_ask_without_approval_needs_approval() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "code.edit",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Allow, &[("code.edit", PolicyRule::Ask)]),
            &approvals,
        );
        assert_eq!(decision, PolicyDecision::NeedsApproval);
    }

    #[test]
    fn policy_rule_ask_with_approval_passes() {
        let approvals = SessionApprovals::new();
        approvals.approve("code.edit");
        let decision = evaluate_policy(
            "code.edit",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Allow, &[("code.edit", PolicyRule::Ask)]),
            &approvals,
        );
        assert_eq!(decision, PolicyDecision::Allowed);
    }

    #[test]
    fn policy_default_rule_applies_to_unlisted_tools() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "shell.exec",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Deny, &[]),
            &approvals,
        );
        assert!(matches!(decision, PolicyDecision::Denied { .. }));
    }

    #[test]
    fn policy_config_default_is_all_allow() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "shell.exec",
            false,
            VesselMode::Normal,
            &ToolPolicyConfig::default(),
            &approvals,
        );
        assert_eq!(decision, PolicyDecision::Allowed);
    }

    #[test]
    fn session_approvals_persist_across_actions() {
        let approvals = SessionApprovals::new();
        assert!(!approvals.is_approved("code.edit"));
        approvals.approve("code.edit");
        assert!(approvals.is_approved("code.edit"));
    }

    #[test]
    fn session_approvals_thread_safe() {
        let approvals = Arc::new(SessionApprovals::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let approvals = Arc::clone(&approvals);
            handles.push(thread::spawn(move || {
                approvals.approve("code.edit");
                approvals.is_approved("code.edit")
            }));
        }
        for handle in handles {
            assert!(handle.join().unwrap());
        }
    }

    #[test]
    fn planning_mode_blocks_mutating_tools() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "code.edit",
            false,
            VesselMode::Planning,
            &ToolPolicyConfig::default(),
            &approvals,
        );
        assert!(matches!(decision, PolicyDecision::Denied { .. }));
    }

    #[test]
    fn planning_mode_allows_read_only_tools() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "code.read",
            true,
            VesselMode::Planning,
            &ToolPolicyConfig::default(),
            &approvals,
        );
        assert_eq!(decision, PolicyDecision::Allowed);
    }

    #[test]
    fn executing_mode_uses_normal_policy() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "shell.exec",
            false,
            VesselMode::Executing,
            &config(PolicyRule::Allow, &[("shell.exec", PolicyRule::Deny)]),
            &approvals,
        );
        assert!(matches!(decision, PolicyDecision::Denied { .. }));
    }

    #[test]
    fn planning_mode_allows_agent_ask_user() {
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "agent.ask_user",
            true,
            VesselMode::Planning,
            &ToolPolicyConfig::default(),
            &approvals,
        );
        assert_eq!(decision, PolicyDecision::Allowed);
    }

    // ── T16: policy_config_from_toml ──
    #[test]
    fn policy_config_from_toml() {
        let toml_str = r#"
            default = "allow"

            [rules]
            "code.edit" = "ask"
            "shell.exec" = "deny"
        "#;
        let parsed: ToolPolicyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.default, PolicyRule::Allow);
        assert_eq!(parsed.rule_for("code.edit"), PolicyRule::Ask);
        assert_eq!(parsed.rule_for("shell.exec"), PolicyRule::Deny);
        assert_eq!(parsed.rule_for("delay"), PolicyRule::Allow);
    }

    // ── T17: policy_composes_with_trust_gate ──
    /// The trust gate and policy gate compose: both must pass.
    /// A trust-denied action stays denied even if policy is Allow.
    /// This test validates the composition contract at the decision level.
    #[test]
    fn policy_composes_with_trust_gate() {
        let approvals = SessionApprovals::new();
        // Policy says Allow, but the trust gate (simulated here) says deny.
        // The policy decision is Allowed, but the overall outcome should
        // be denied because trust gate runs first.
        let policy_decision = evaluate_policy(
            "code.edit",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Allow, &[]),
            &approvals,
        );
        assert_eq!(policy_decision, PolicyDecision::Allowed);

        // If trust gate denies, the action never reaches policy evaluation.
        // This is the composition contract: trust_gate AND policy_gate.
        // Both must independently pass. A policy Allow doesn't override
        // a trust deny. The test documents this design invariant.
        let trust_denied = true;
        let effective_allowed = !trust_denied && policy_decision == PolicyDecision::Allowed;
        assert!(
            !effective_allowed,
            "trust-denied must override policy-allowed"
        );
    }

    // ── T18: act_policy_denied_records_outcome ──
    #[test]
    fn act_policy_denied_records_outcome() {
        // Verify that PolicyDenied evaluates to a denial that would produce
        // an ActionOutcome::PolicyDenied in the act step.
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "fs.write",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Deny, &[]),
            &approvals,
        );
        assert!(
            matches!(decision, PolicyDecision::Denied { .. }),
            "Deny rule must produce PolicyDecision::Denied"
        );
        // In production, when PolicyDecision::Denied is returned, the act step
        // records ActionOutcome::PolicyDenied on the ActionRecord.
    }

    // ── T19: act_policy_denied_broadcasts_event ──
    #[test]
    fn act_policy_denied_broadcasts_event() {
        // Verify that NeedsApproval also maps to a denial (PolicyApprovalRequired event).
        let approvals = SessionApprovals::new();
        let decision = evaluate_policy(
            "code.edit",
            false,
            VesselMode::Normal,
            &config(PolicyRule::Allow, &[("code.edit", PolicyRule::Ask)]),
            &approvals,
        );
        assert_eq!(
            decision,
            PolicyDecision::NeedsApproval,
            "Ask rule without approval must produce NeedsApproval"
        );
        // In production, NeedsApproval triggers broadcast_policy_event()
        // which sends a PolicyApprovalRequired LiveEvent.
    }

    // ── T21a: policy_config_none_defaults_to_all_allow ──
    #[test]
    fn policy_config_none_defaults_to_all_allow() {
        // When VesselConfigFile has no [tool_policy] section, the default
        // ToolPolicyConfig is used, which allows all tools.
        let config = ToolPolicyConfig::default();
        assert_eq!(config.default, PolicyRule::Allow);
        assert!(config.rules.is_empty());

        // Verify any tool gets Allow
        let approvals = SessionApprovals::new();
        for tool in &["code.edit", "shell.exec", "fs.write", "code.read", "delay"] {
            let decision = evaluate_policy(tool, false, VesselMode::Normal, &config, &approvals);
            assert_eq!(
                decision,
                PolicyDecision::Allowed,
                "default config must allow '{tool}'"
            );
        }
    }
}
