//! Budget configuration and state types for dual-engine enforcement (I6, I9).
//!
//! Defines the configuration types that govern budget enforcement across both
//! the Cognitive AQ and Tool AQ engines, as well as the state types used for
//! tracking and reporting.

use serde::{Deserialize, Serialize};

use crate::ExoError;

/// Configuration for Cognitive AQ budgets.
///
/// All budgets are enforced per time window. When a window expires, budgets are
/// replenished. This prevents unbounded accumulation while allowing bursty usage
/// within each window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitiveBudgetConfig {
    /// Total local model tokens per window (input + output combined).
    pub local_token_budget: u64,
    /// Total frontier model tokens per window (input + output combined).
    pub frontier_token_budget: u64,
    /// Maximum frontier spend per window in hundredths of a cent.
    pub frontier_cost_budget_cents: u64,
    /// Budget window duration in seconds (e.g., 3600 = hourly).
    pub time_window_secs: u64,
    /// Maximum tokens per single tick (prevents runaway single-tick consumption).
    pub per_tick_token_cap: u64,
    /// Maximum tokens per single thread invocation per tick.
    pub per_thread_token_cap: u64,
    /// Model escalation policy.
    pub escalation_policy: EscalationPolicy,
}

impl Default for CognitiveBudgetConfig {
    fn default() -> Self {
        Self {
            local_token_budget: 1_000_000,
            frontier_token_budget: 100_000,
            frontier_cost_budget_cents: 500,
            time_window_secs: 3600,
            per_tick_token_cap: 50_000,
            per_thread_token_cap: 10_000,
            escalation_policy: EscalationPolicy::default(),
        }
    }
}

impl CognitiveBudgetConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), ExoError> {
        if self.time_window_secs == 0 {
            return Err(ExoError::Config("time_window_secs must be > 0".into()));
        }
        if self.local_token_budget == 0 && self.frontier_token_budget == 0 {
            return Err(ExoError::Config(
                "at least one token budget (local or frontier) must be > 0".into(),
            ));
        }
        if self.per_tick_token_cap == 0 {
            return Err(ExoError::Config("per_tick_token_cap must be > 0".into()));
        }
        if self.per_thread_token_cap == 0 {
            return Err(ExoError::Config("per_thread_token_cap must be > 0".into()));
        }
        self.escalation_policy.validate()?;
        Ok(())
    }
}

/// Configuration for tool execution rate limits.
///
/// Enforced independently of cognitive budgets (I6, I9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolBudgetConfig {
    /// Maximum tool invocations per time window.
    pub max_invocations_per_window: u64,
    /// Time window duration in seconds.
    pub time_window_secs: u64,
}

impl Default for ToolBudgetConfig {
    fn default() -> Self {
        Self {
            max_invocations_per_window: 1000,
            time_window_secs: 3600,
        }
    }
}

impl ToolBudgetConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), ExoError> {
        if self.time_window_secs == 0 {
            return Err(ExoError::Config(
                "tool budget time_window_secs must be > 0".into(),
            ));
        }
        if self.max_invocations_per_window == 0 {
            return Err(ExoError::Config(
                "max_invocations_per_window must be > 0".into(),
            ));
        }
        Ok(())
    }
}

/// Policy for escalating from local to frontier model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationPolicy {
    /// Self-Critique uncertainty threshold for frontier escalation (0.0-1.0).
    pub escalate_on_uncertainty: f64,
    /// Action types that always use frontier model.
    #[serde(default)]
    pub escalate_on_stakes: Vec<String>,
    /// Number of consecutive failures before trying frontier.
    pub escalate_on_consecutive_failures: u32,
    /// Maximum frontier calls per window (independent of token budget).
    pub max_frontier_calls_per_window: u64,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            escalate_on_uncertainty: 0.7,
            escalate_on_stakes: vec![],
            escalate_on_consecutive_failures: 3,
            max_frontier_calls_per_window: 50,
        }
    }
}

impl EscalationPolicy {
    /// Validate the policy.
    pub fn validate(&self) -> Result<(), ExoError> {
        if !(0.0..=1.0).contains(&self.escalate_on_uncertainty) {
            return Err(ExoError::Config(
                "escalate_on_uncertainty must be between 0.0 and 1.0".into(),
            ));
        }
        Ok(())
    }
}

/// Snapshot of budget state across all tracked dimensions.
///
/// Used for reporting in StateSnapshot and for thrash detection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetState {
    /// Per-dimension tracking.
    pub dimensions: Vec<BudgetDimensionState>,
    /// Current window start time.
    pub window_start: chrono::DateTime<chrono::Utc>,
    /// Window duration in seconds.
    pub window_secs: u64,
    /// Number of frontier calls made this window.
    pub frontier_calls_this_window: u64,
    /// Consecutive tick failures (for escalation).
    pub consecutive_failures: u32,
}

/// State of a single budget dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetDimensionState {
    /// Name of this dimension (e.g., "local_tokens", "frontier_tokens").
    pub dimension: String,
    /// Budget limit for this window.
    pub limit: u64,
    /// Amount consumed so far in this window.
    pub consumed: u64,
    /// Amount remaining (limit - consumed, saturating).
    pub remaining: u64,
    /// Whether this dimension is exhausted.
    pub exhausted: bool,
}

/// Thrash severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThrashLevel {
    /// No thrashing detected.
    None,
    /// Low-level repetition — log warning, consider backoff.
    Low,
    /// Medium-level thrashing — escalate model and reduce scope.
    Medium,
    /// High-level thrashing — suspend cognitive loop and alert human.
    High,
}

/// Result of thrash detection analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThrashAssessment {
    /// Severity level of detected thrashing.
    pub level: ThrashLevel,
    /// Human-readable descriptions of thrash indicators.
    pub indicators: Vec<String>,
    /// Recommended corrective action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
}

impl ThrashAssessment {
    /// No thrashing detected.
    pub fn none() -> Self {
        Self {
            level: ThrashLevel::None,
            indicators: vec![],
            recommendation: None,
        }
    }
}

/// Budget state that persists across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedBudgetState {
    /// When the current window started.
    pub window_start: chrono::DateTime<chrono::Utc>,
    /// Local model tokens consumed this window.
    pub local_tokens_consumed: u64,
    /// Frontier model tokens consumed this window.
    pub frontier_tokens_consumed: u64,
    /// Frontier cost consumed this window (hundredths of a cent).
    pub frontier_cost_consumed: u64,
    /// Frontier calls made this window.
    pub frontier_calls: u64,
    /// Per-thread consumption (ThreadId serialized as string → tokens).
    pub per_thread_consumed: std::collections::HashMap<String, u64>,
    /// Consecutive tick failures.
    pub consecutive_failures: u32,
    /// Tool invocations this window.
    pub tool_invocations: u64,
}

/// Persistence layer for budget state.
///
/// Simple key-value store for budget window state. The full state
/// is serialized as JSON and stored under a well-known key.
pub trait BudgetStore: Send + Sync {
    /// Save the current budget state.
    fn save(&self, state: &PersistedBudgetState) -> Result<(), ExoError>;
    /// Load the most recently saved budget state.
    fn load(&self) -> Result<Option<PersistedBudgetState>, ExoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-1: Core budget type tests ──

    #[test]
    fn cognitive_budget_config_defaults() {
        let config = CognitiveBudgetConfig::default();
        assert_eq!(config.local_token_budget, 1_000_000);
        assert_eq!(config.frontier_token_budget, 100_000);
        assert_eq!(config.frontier_cost_budget_cents, 500);
        assert_eq!(config.time_window_secs, 3600);
        assert_eq!(config.per_tick_token_cap, 50_000);
        assert_eq!(config.per_thread_token_cap, 10_000);
    }

    #[test]
    fn tool_budget_config_defaults() {
        let config = ToolBudgetConfig::default();
        assert_eq!(config.max_invocations_per_window, 1000);
        assert_eq!(config.time_window_secs, 3600);
    }

    #[test]
    fn escalation_policy_defaults() {
        let policy = EscalationPolicy::default();
        assert!((policy.escalate_on_uncertainty - 0.7).abs() < f64::EPSILON);
        assert!(policy.escalate_on_stakes.is_empty());
        assert_eq!(policy.escalate_on_consecutive_failures, 3);
        assert_eq!(policy.max_frontier_calls_per_window, 50);
    }

    #[test]
    fn cognitive_config_serialization_roundtrip() {
        let config = CognitiveBudgetConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: CognitiveBudgetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn tool_config_serialization_roundtrip() {
        let config = ToolBudgetConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ToolBudgetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn escalation_policy_serialization_roundtrip() {
        let policy = EscalationPolicy {
            escalate_on_uncertainty: 0.5,
            escalate_on_stakes: vec!["fs.write".into(), "http.request".into()],
            escalate_on_consecutive_failures: 5,
            max_frontier_calls_per_window: 25,
        };
        let json = serde_json::to_string(&policy).unwrap();
        let parsed: EscalationPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, parsed);
    }

    #[test]
    fn thrash_level_ordering() {
        assert!(ThrashLevel::None < ThrashLevel::Low);
        assert!(ThrashLevel::Low < ThrashLevel::Medium);
        assert!(ThrashLevel::Medium < ThrashLevel::High);
    }

    #[test]
    fn thrash_level_serialization_roundtrip() {
        for level in [
            ThrashLevel::None,
            ThrashLevel::Low,
            ThrashLevel::Medium,
            ThrashLevel::High,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let parsed: ThrashLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, parsed);
        }
    }

    #[test]
    fn thrash_assessment_none() {
        let assessment = ThrashAssessment::none();
        assert_eq!(assessment.level, ThrashLevel::None);
        assert!(assessment.indicators.is_empty());
        assert!(assessment.recommendation.is_none());
    }

    #[test]
    fn budget_dimension_state_serialization_roundtrip() {
        let state = BudgetDimensionState {
            dimension: "local_tokens".into(),
            limit: 1_000_000,
            consumed: 250_000,
            remaining: 750_000,
            exhausted: false,
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: BudgetDimensionState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, parsed);
    }

    #[test]
    fn cognitive_config_validation_zero_window() {
        let config = CognitiveBudgetConfig {
            time_window_secs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn cognitive_config_validation_zero_budgets() {
        let config = CognitiveBudgetConfig {
            local_token_budget: 0,
            frontier_token_budget: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn cognitive_config_validation_zero_tick_cap() {
        let config = CognitiveBudgetConfig {
            per_tick_token_cap: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn cognitive_config_validation_zero_thread_cap() {
        let config = CognitiveBudgetConfig {
            per_thread_token_cap: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn tool_config_validation_zero_window() {
        let config = ToolBudgetConfig {
            time_window_secs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn tool_config_validation_zero_invocations() {
        let config = ToolBudgetConfig {
            max_invocations_per_window: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn escalation_policy_validation_out_of_range() {
        let policy = EscalationPolicy {
            escalate_on_uncertainty: 1.5,
            ..Default::default()
        };
        assert!(policy.validate().is_err());
        let policy = EscalationPolicy {
            escalate_on_uncertainty: -0.1,
            ..Default::default()
        };
        assert!(policy.validate().is_err());
    }

    #[test]
    fn persisted_budget_state_roundtrip() {
        let state = PersistedBudgetState {
            window_start: chrono::Utc::now(),
            local_tokens_consumed: 50_000,
            frontier_tokens_consumed: 10_000,
            frontier_cost_consumed: 100,
            frontier_calls: 5,
            per_thread_consumed: {
                let mut m = std::collections::HashMap::new();
                m.insert("thread-1".into(), 3000);
                m.insert("thread-2".into(), 1500);
                m
            },
            consecutive_failures: 2,
            tool_invocations: 15,
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: PersistedBudgetState = serde_json::from_str(&json).unwrap();
        assert_eq!(state.local_tokens_consumed, parsed.local_tokens_consumed);
        assert_eq!(state.frontier_calls, parsed.frontier_calls);
        assert_eq!(state.consecutive_failures, parsed.consecutive_failures);
    }
}
