//! Budget enforcement for the Exoskeleton runtime (I6, I9).
//!
//! Two-layer enforcement on each of two independent engines:
//! - **Cognitive AQ**: `CognitiveBudgetTracker` (Exo layer) + AQ `BudgetGate` (hard stop)
//! - **Tool AQ**: `ToolBudgetGate` (Act step boundary) + WI Host `dispatch_concurrency`

pub mod thrash;
pub mod tracker;

use chrono::{DateTime, Utc};
use exoskeleton_core::budget::ToolBudgetConfig;
use serde::{Deserialize, Serialize};
pub use thrash::ThrashDetector;
pub use tracker::CognitiveBudgetTracker;

/// Rate-limits tool invocations at the Act step boundary.
///
/// Enforced independently of cognitive budgets (I6, I9). The WI Host's
/// `dispatch_concurrency` provides a second enforcement layer for concurrent
/// execution limits.
#[derive(Debug)]
pub struct ToolBudgetGate {
    config: ToolBudgetConfig,
    invocations_this_window: u64,
    window_start: DateTime<Utc>,
}

impl ToolBudgetGate {
    /// Create a new gate with the given configuration.
    pub fn new(config: ToolBudgetConfig) -> Self {
        Self {
            config,
            invocations_this_window: 0,
            window_start: Utc::now(),
        }
    }

    /// Check if an invocation is allowed (true = allowed).
    pub fn check(&self) -> bool {
        self.invocations_this_window < self.config.max_invocations_per_window
    }

    /// Record a tool invocation.
    pub fn record_invocation(&mut self) {
        self.invocations_this_window = self.invocations_this_window.saturating_add(1);
    }

    /// Remaining invocations in this window.
    pub fn remaining(&self) -> u64 {
        self.config
            .max_invocations_per_window
            .saturating_sub(self.invocations_this_window)
    }

    /// Reset the window counters.
    pub fn reset_window(&mut self) {
        self.invocations_this_window = 0;
        self.window_start = Utc::now();
    }

    /// Current invocation count this window.
    pub fn invocations_this_window(&self) -> u64 {
        self.invocations_this_window
    }

    /// Restore state from persisted budget (on restart).
    pub fn restore(&mut self, invocations: u64, window_start: DateTime<Utc>) {
        self.invocations_this_window = invocations;
        self.window_start = window_start;
    }

    /// The window start time.
    pub fn window_start(&self) -> DateTime<Utc> {
        self.window_start
    }
}

/// Consumption data from a single thread's LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadLlmConsumption {
    /// Which thread produced this consumption.
    pub thread_id: exoskeleton_core::ThreadId,
    /// Total tokens consumed (input + output).
    pub tokens: u64,
    /// Cost in hundredths of a cent.
    pub cost_cents: f64,
    /// Which backend was used.
    pub backend: exoskeleton_core::LlmBackend,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-3: ToolBudgetGate tests ──

    #[test]
    fn gate_check_allowed_when_under_limit() {
        let config = ToolBudgetConfig {
            max_invocations_per_window: 5,
            time_window_secs: 3600,
        };
        let gate = ToolBudgetGate::new(config);
        assert!(gate.check());
        assert_eq!(gate.remaining(), 5);
    }

    #[test]
    fn gate_check_blocked_when_at_limit() {
        let config = ToolBudgetConfig {
            max_invocations_per_window: 2,
            time_window_secs: 3600,
        };
        let mut gate = ToolBudgetGate::new(config);
        gate.record_invocation();
        gate.record_invocation();
        assert!(!gate.check());
        assert_eq!(gate.remaining(), 0);
    }

    #[test]
    fn gate_record_invocation_increments() {
        let config = ToolBudgetConfig {
            max_invocations_per_window: 10,
            time_window_secs: 3600,
        };
        let mut gate = ToolBudgetGate::new(config);
        assert_eq!(gate.invocations_this_window(), 0);
        gate.record_invocation();
        assert_eq!(gate.invocations_this_window(), 1);
        assert_eq!(gate.remaining(), 9);
    }

    #[test]
    fn gate_reset_window_clears_counter() {
        let config = ToolBudgetConfig {
            max_invocations_per_window: 3,
            time_window_secs: 3600,
        };
        let mut gate = ToolBudgetGate::new(config);
        gate.record_invocation();
        gate.record_invocation();
        gate.record_invocation();
        assert!(!gate.check());

        gate.reset_window();
        assert!(gate.check());
        assert_eq!(gate.remaining(), 3);
        assert_eq!(gate.invocations_this_window(), 0);
    }

    #[test]
    fn gate_remaining_calculation() {
        let config = ToolBudgetConfig {
            max_invocations_per_window: 100,
            time_window_secs: 3600,
        };
        let mut gate = ToolBudgetGate::new(config);
        for _ in 0..37 {
            gate.record_invocation();
        }
        assert_eq!(gate.remaining(), 63);
    }
}
