//! Cognitive budget tracker — Exoskeleton-layer budget enforcement.
//!
//! Complements AQ's BudgetTracker (which handles the hard dispatch gate) with:
//! - Separate local/frontier token tracking
//! - Per-thread token caps
//! - Time-windowed budget management
//! - Consecutive failure counting (for escalation)
//!
//! Persisted to SQLite via BudgetStore (survives restarts).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use exoskeleton_core::budget::{
    BudgetDimensionState, BudgetStore, CognitiveBudgetConfig, PersistedBudgetState, ThrashLevel,
};
use exoskeleton_core::snapshot::BudgetStatus;
use exoskeleton_core::{ExoError, LlmBackend, ThreadId};

/// Tracks cognitive budget consumption at the Exoskeleton level.
pub struct CognitiveBudgetTracker {
    config: CognitiveBudgetConfig,
    state: CognitiveBudgetState,
    store: Arc<dyn BudgetStore>,
    /// Current thrash level (set externally by ThrashDetector).
    thrash_level: ThrashLevel,
}

/// In-memory state for the current budget window.
struct CognitiveBudgetState {
    window_start: DateTime<Utc>,
    local_tokens_consumed: u64,
    frontier_tokens_consumed: u64,
    frontier_cost_consumed: u64,
    frontier_calls: u64,
    per_thread_consumed: HashMap<ThreadId, u64>,
    consecutive_failures: u32,
    tick_tokens_this_tick: u64,
}

impl CognitiveBudgetState {
    fn new() -> Self {
        Self {
            window_start: Utc::now(),
            local_tokens_consumed: 0,
            frontier_tokens_consumed: 0,
            frontier_cost_consumed: 0,
            frontier_calls: 0,
            per_thread_consumed: HashMap::new(),
            consecutive_failures: 0,
            tick_tokens_this_tick: 0,
        }
    }
}

/// Result of a cognitive budget check.
#[derive(Debug, Clone, PartialEq)]
pub enum CognitiveBudgetCheck {
    /// Budget available, proceed with this backend.
    Available(LlmBackend),
    /// Budget exhausted for requested backend, try this fallback.
    Fallback(LlmBackend),
    /// All budgets exhausted — suspend.
    Exhausted,
}

impl CognitiveBudgetTracker {
    /// Create a new tracker with the given config and persistence store.
    pub fn new(config: CognitiveBudgetConfig, store: Arc<dyn BudgetStore>) -> Self {
        Self {
            config,
            state: CognitiveBudgetState::new(),
            store,
            thrash_level: ThrashLevel::None,
        }
    }

    /// Load state from the persistence store on restart.
    /// If the stored window has expired, resets to a fresh window.
    pub fn load_or_reset(&mut self) -> Result<(), ExoError> {
        if let Some(persisted) = self.store.load()? {
            let window_duration = chrono::Duration::seconds(self.config.time_window_secs as i64);
            let window_end = persisted.window_start + window_duration;

            if Utc::now() < window_end {
                // Window still active — restore state
                self.state.window_start = persisted.window_start;
                self.state.local_tokens_consumed = persisted.local_tokens_consumed;
                self.state.frontier_tokens_consumed = persisted.frontier_tokens_consumed;
                self.state.frontier_cost_consumed = persisted.frontier_cost_consumed;
                self.state.frontier_calls = persisted.frontier_calls;
                self.state.consecutive_failures = persisted.consecutive_failures;

                // Restore per-thread consumption (parse ThreadId from string)
                self.state.per_thread_consumed.clear();
                for (tid_str, tokens) in &persisted.per_thread_consumed {
                    if let Ok(tid) = tid_str.parse::<ThreadId>() {
                        self.state.per_thread_consumed.insert(tid, *tokens);
                    }
                }

                tracing::info!(
                    window_start = %persisted.window_start,
                    local_consumed = persisted.local_tokens_consumed,
                    frontier_consumed = persisted.frontier_tokens_consumed,
                    "budget state restored from store"
                );
            } else {
                // Window expired during downtime — start fresh
                self.state = CognitiveBudgetState::new();
                // Keep consecutive failures across window resets
                self.state.consecutive_failures = persisted.consecutive_failures;
                tracing::info!("budget window expired during downtime; starting fresh window");
            }
        }
        Ok(())
    }

    /// Record an LLM call's consumption.
    pub fn record_llm_call(
        &mut self,
        backend: LlmBackend,
        tokens_in: u64,
        tokens_out: u64,
        cost_cents: f64,
    ) {
        let total_tokens = tokens_in.saturating_add(tokens_out);
        match backend {
            LlmBackend::Local => {
                self.state.local_tokens_consumed = self
                    .state
                    .local_tokens_consumed
                    .saturating_add(total_tokens);
            }
            LlmBackend::Frontier => {
                self.state.frontier_tokens_consumed = self
                    .state
                    .frontier_tokens_consumed
                    .saturating_add(total_tokens);
                let cost_hundredths = (cost_cents * 100.0).round() as u64;
                self.state.frontier_cost_consumed = self
                    .state
                    .frontier_cost_consumed
                    .saturating_add(cost_hundredths);
                self.state.frontier_calls = self.state.frontier_calls.saturating_add(1);
            }
        }
        self.state.tick_tokens_this_tick = self
            .state
            .tick_tokens_this_tick
            .saturating_add(total_tokens);
    }

    /// Record a thread's LLM token consumption for per-thread cap enforcement.
    pub fn record_thread_consumption(&mut self, thread_id: ThreadId, tokens: u64) {
        let entry = self.state.per_thread_consumed.entry(thread_id).or_insert(0);
        *entry = entry.saturating_add(tokens);
    }

    /// Check if there is remaining budget for an LLM call.
    pub fn check_cognitive_budget(&self) -> CognitiveBudgetCheck {
        let local_remaining = self.remaining_local_tokens();
        let frontier_remaining = self.remaining_frontier_tokens();

        if local_remaining > 0 {
            CognitiveBudgetCheck::Available(LlmBackend::Local)
        } else if frontier_remaining > 0 && self.remaining_frontier_cost() > 0 {
            CognitiveBudgetCheck::Fallback(LlmBackend::Frontier)
        } else {
            CognitiveBudgetCheck::Exhausted
        }
    }

    /// Check if a specific thread has remaining per-thread budget.
    /// Per-thread budget is per-invocation cap (not cumulative across ticks).
    pub fn check_thread_budget(&self, thread_id: ThreadId) -> bool {
        // Per-thread budget is a per-window cumulative cap
        let consumed = self
            .state
            .per_thread_consumed
            .get(&thread_id)
            .copied()
            .unwrap_or(0);
        consumed < self.config.per_thread_token_cap
    }

    /// Check if the per-tick token cap has been exceeded.
    pub fn check_tick_cap(&self) -> bool {
        self.state.tick_tokens_this_tick < self.config.per_tick_token_cap
    }

    /// Reset per-tick counters (called at start of each tick).
    pub fn reset_tick_counters(&mut self) {
        self.state.tick_tokens_this_tick = 0;
    }

    /// Reset the budget window (called by the window timer).
    pub fn reset_window(&mut self) -> Result<(), ExoError> {
        self.state.window_start = Utc::now();
        self.state.local_tokens_consumed = 0;
        self.state.frontier_tokens_consumed = 0;
        self.state.frontier_cost_consumed = 0;
        self.state.frontier_calls = 0;
        self.state.per_thread_consumed.clear();
        self.state.tick_tokens_this_tick = 0;
        // consecutive_failures persists across windows
        self.persist()
    }

    /// Record a tick failure for consecutive failure tracking.
    pub fn record_failure(&mut self) {
        self.state.consecutive_failures = self.state.consecutive_failures.saturating_add(1);
    }

    /// Clear the consecutive failure counter (called on successful tick).
    pub fn clear_failures(&mut self) {
        self.state.consecutive_failures = 0;
    }

    /// Build the BudgetStatus for the StateSnapshot.
    pub fn budget_status(&self, tool_invocations_remaining: u64) -> BudgetStatus {
        let window_duration = chrono::Duration::seconds(self.config.time_window_secs as i64);
        let window_end = self.state.window_start + window_duration;
        let time_remaining = (window_end - Utc::now()).num_seconds().max(0) as u64;

        BudgetStatus {
            local_tokens_remaining: self.remaining_local_tokens(),
            frontier_tokens_remaining: self.remaining_frontier_tokens(),
            frontier_cost_cents_remaining: self.remaining_frontier_cost(),
            time_secs_remaining: time_remaining,
            thrash_level: self.thrash_level,
            tool_invocations_remaining,
        }
    }

    /// Build AQ BudgetConsumption records for HandlerOutput.
    /// Returns consumption since last reset_tick_counters() call.
    pub fn build_consumption(&self) -> Vec<actionqueue_core::budget::BudgetConsumption> {
        let total_tokens = self.state.tick_tokens_this_tick;
        let mut consumption = Vec::new();

        if total_tokens > 0 {
            consumption.push(actionqueue_core::budget::BudgetConsumption {
                dimension: actionqueue_core::budget::BudgetDimension::Token,
                amount: total_tokens,
            });
        }

        // Cost consumption — approximate from frontier tokens this tick
        // We track cost at the Exo layer; AQ sees it aggregated as CostCents
        let frontier_cost_this_tick = self.state.frontier_cost_consumed; // cumulative, but AQ handles delta
        if frontier_cost_this_tick > 0 {
            // Note: AQ tracks cumulative; we only report this tick's tokens
            // The AQ cost tracking is handled separately via the CostCents dimension
        }

        consumption
    }

    /// Persist current state to the store.
    pub fn persist(&self) -> Result<(), ExoError> {
        let persisted = PersistedBudgetState {
            window_start: self.state.window_start,
            local_tokens_consumed: self.state.local_tokens_consumed,
            frontier_tokens_consumed: self.state.frontier_tokens_consumed,
            frontier_cost_consumed: self.state.frontier_cost_consumed,
            frontier_calls: self.state.frontier_calls,
            per_thread_consumed: self
                .state
                .per_thread_consumed
                .iter()
                .map(|(tid, tokens)| (tid.to_string(), *tokens))
                .collect(),
            consecutive_failures: self.state.consecutive_failures,
            tool_invocations: 0, // Tool budget tracked separately
        };
        self.store.save(&persisted)
    }

    /// Remaining local model tokens this window.
    pub fn remaining_local_tokens(&self) -> u64 {
        self.config
            .local_token_budget
            .saturating_sub(self.state.local_tokens_consumed)
    }

    /// Remaining frontier model tokens this window.
    pub fn remaining_frontier_tokens(&self) -> u64 {
        self.config
            .frontier_token_budget
            .saturating_sub(self.state.frontier_tokens_consumed)
    }

    /// Remaining frontier cost (hundredths of a cent) this window.
    pub fn remaining_frontier_cost(&self) -> u64 {
        self.config
            .frontier_cost_budget_cents
            .saturating_sub(self.state.frontier_cost_consumed)
    }

    /// Frontier calls made this window.
    pub fn frontier_calls_this_window(&self) -> u64 {
        self.state.frontier_calls
    }

    /// Consecutive failure count.
    pub fn consecutive_failures(&self) -> u32 {
        self.state.consecutive_failures
    }

    /// Access the configuration.
    pub fn config(&self) -> &CognitiveBudgetConfig {
        &self.config
    }

    /// Set the current thrash level (from ThrashDetector).
    pub fn set_thrash_level(&mut self, level: ThrashLevel) {
        self.thrash_level = level;
    }

    /// Get the current thrash level.
    pub fn thrash_level(&self) -> ThrashLevel {
        self.thrash_level
    }

    /// Window start time.
    pub fn window_start(&self) -> DateTime<Utc> {
        self.state.window_start
    }

    /// Build a BudgetState for reporting.
    pub fn budget_state(&self) -> exoskeleton_core::budget::BudgetState {
        exoskeleton_core::budget::BudgetState {
            dimensions: vec![
                BudgetDimensionState {
                    dimension: "local_tokens".into(),
                    limit: self.config.local_token_budget,
                    consumed: self.state.local_tokens_consumed,
                    remaining: self.remaining_local_tokens(),
                    exhausted: self.remaining_local_tokens() == 0,
                },
                BudgetDimensionState {
                    dimension: "frontier_tokens".into(),
                    limit: self.config.frontier_token_budget,
                    consumed: self.state.frontier_tokens_consumed,
                    remaining: self.remaining_frontier_tokens(),
                    exhausted: self.remaining_frontier_tokens() == 0,
                },
                BudgetDimensionState {
                    dimension: "frontier_cost_cents".into(),
                    limit: self.config.frontier_cost_budget_cents,
                    consumed: self.state.frontier_cost_consumed,
                    remaining: self.remaining_frontier_cost(),
                    exhausted: self.remaining_frontier_cost() == 0,
                },
            ],
            window_start: self.state.window_start,
            window_secs: self.config.time_window_secs,
            frontier_calls_this_window: self.state.frontier_calls,
            consecutive_failures: self.state.consecutive_failures,
        }
    }
}

/// In-memory budget store for testing.
pub struct InMemoryBudgetStore {
    state: std::sync::Mutex<Option<PersistedBudgetState>>,
}

impl Default for InMemoryBudgetStore {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(None),
        }
    }
}

impl InMemoryBudgetStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BudgetStore for InMemoryBudgetStore {
    fn save(&self, state: &PersistedBudgetState) -> Result<(), ExoError> {
        *self.state.lock().unwrap() = Some(state.clone());
        Ok(())
    }

    fn load(&self) -> Result<Option<PersistedBudgetState>, ExoError> {
        Ok(self.state.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CognitiveBudgetConfig {
        CognitiveBudgetConfig {
            local_token_budget: 10_000,
            frontier_token_budget: 5_000,
            frontier_cost_budget_cents: 100,
            time_window_secs: 3600,
            per_tick_token_cap: 2_000,
            per_thread_token_cap: 500,
            escalation_policy: Default::default(),
        }
    }

    fn test_tracker() -> CognitiveBudgetTracker {
        let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
        CognitiveBudgetTracker::new(test_config(), store)
    }

    // ── T-2: CognitiveBudgetTracker tests ──

    #[test]
    fn new_tracker_has_full_budget() {
        let tracker = test_tracker();
        assert_eq!(tracker.remaining_local_tokens(), 10_000);
        assert_eq!(tracker.remaining_frontier_tokens(), 5_000);
        assert_eq!(tracker.remaining_frontier_cost(), 100);
        assert_eq!(tracker.consecutive_failures(), 0);
    }

    #[test]
    fn record_local_llm_call_tracks_consumption() {
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 200, 100, 0.0);
        assert_eq!(tracker.remaining_local_tokens(), 9_700);
        assert_eq!(tracker.remaining_frontier_tokens(), 5_000); // unaffected
    }

    #[test]
    fn record_frontier_llm_call_tracks_all_dimensions() {
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Frontier, 300, 200, 0.50);
        assert_eq!(tracker.remaining_frontier_tokens(), 4_500);
        assert_eq!(tracker.remaining_frontier_cost(), 50); // 100 - 50
        assert_eq!(tracker.frontier_calls_this_window(), 1);
        assert_eq!(tracker.remaining_local_tokens(), 10_000); // unaffected
    }

    #[test]
    fn per_thread_budget_check() {
        let mut tracker = test_tracker();
        let thread_id = ThreadId::new();

        // No consumption → within budget
        assert!(tracker.check_thread_budget(thread_id));

        // Consume within cap (500)
        tracker.record_thread_consumption(thread_id, 400);
        assert!(tracker.check_thread_budget(thread_id));

        // Consume past cap
        tracker.record_thread_consumption(thread_id, 200);
        assert!(!tracker.check_thread_budget(thread_id));
    }

    #[test]
    fn per_tick_cap_enforcement() {
        let mut tracker = test_tracker();
        assert!(tracker.check_tick_cap());

        // Consume within cap (2000)
        tracker.record_llm_call(LlmBackend::Local, 1000, 500, 0.0);
        assert!(tracker.check_tick_cap());

        // Consume past cap
        tracker.record_llm_call(LlmBackend::Local, 500, 200, 0.0);
        assert!(!tracker.check_tick_cap());
    }

    #[test]
    fn reset_tick_counters() {
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 1500, 600, 0.0);
        assert!(!tracker.check_tick_cap());

        tracker.reset_tick_counters();
        assert!(tracker.check_tick_cap());
    }

    #[test]
    fn window_reset_clears_all_but_failures() {
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 5000, 3000, 0.0);
        tracker.record_llm_call(LlmBackend::Frontier, 2000, 1000, 0.30);
        tracker.record_failure();
        tracker.record_failure();
        let thread_id = ThreadId::new();
        tracker.record_thread_consumption(thread_id, 300);

        tracker.reset_window().unwrap();

        assert_eq!(tracker.remaining_local_tokens(), 10_000);
        assert_eq!(tracker.remaining_frontier_tokens(), 5_000);
        assert_eq!(tracker.remaining_frontier_cost(), 100);
        assert_eq!(tracker.frontier_calls_this_window(), 0);
        assert!(tracker.check_thread_budget(thread_id));
        // Failures persist across windows
        assert_eq!(tracker.consecutive_failures(), 2);
    }

    #[test]
    fn persist_and_load_preserves_state() {
        let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
        let config = test_config();

        // Consume some budget and persist
        {
            let mut tracker = CognitiveBudgetTracker::new(config.clone(), Arc::clone(&store));
            tracker.record_llm_call(LlmBackend::Local, 3000, 1000, 0.0);
            tracker.record_llm_call(LlmBackend::Frontier, 1000, 500, 0.25);
            tracker.record_failure();
            tracker.persist().unwrap();
        }

        // Load into a new tracker
        {
            let mut tracker = CognitiveBudgetTracker::new(config, Arc::clone(&store));
            tracker.load_or_reset().unwrap();
            assert_eq!(tracker.remaining_local_tokens(), 6_000); // 10000 - 4000
            assert_eq!(tracker.remaining_frontier_tokens(), 3_500); // 5000 - 1500
            assert_eq!(tracker.consecutive_failures(), 1);
        }
    }

    #[test]
    fn consecutive_failure_tracking() {
        let mut tracker = test_tracker();
        assert_eq!(tracker.consecutive_failures(), 0);

        tracker.record_failure();
        assert_eq!(tracker.consecutive_failures(), 1);

        tracker.record_failure();
        assert_eq!(tracker.consecutive_failures(), 2);

        tracker.clear_failures();
        assert_eq!(tracker.consecutive_failures(), 0);
    }

    #[test]
    fn budget_status_reflects_state() {
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 2000, 1000, 0.0);
        tracker.record_llm_call(LlmBackend::Frontier, 500, 300, 0.10);

        let status = tracker.budget_status(950);
        assert_eq!(status.local_tokens_remaining, 7_000);
        assert_eq!(status.frontier_tokens_remaining, 4_200);
        assert_eq!(status.tool_invocations_remaining, 950);
    }

    #[test]
    fn check_cognitive_budget_scenarios() {
        // Fresh tracker → available local
        let tracker = test_tracker();
        assert_eq!(
            tracker.check_cognitive_budget(),
            CognitiveBudgetCheck::Available(LlmBackend::Local)
        );

        // Local exhausted → fallback to frontier
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 5000, 5000, 0.0);
        assert_eq!(
            tracker.check_cognitive_budget(),
            CognitiveBudgetCheck::Fallback(LlmBackend::Frontier)
        );

        // Both exhausted → exhausted
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 5000, 5000, 0.0);
        tracker.record_llm_call(LlmBackend::Frontier, 3000, 2000, 0.0);
        assert_eq!(
            tracker.check_cognitive_budget(),
            CognitiveBudgetCheck::Exhausted
        );
    }

    #[test]
    fn saturating_arithmetic_extreme_values() {
        let config = CognitiveBudgetConfig {
            local_token_budget: 100,
            frontier_token_budget: 100,
            frontier_cost_budget_cents: 100,
            time_window_secs: 3600,
            per_tick_token_cap: 100,
            per_thread_token_cap: 100,
            escalation_policy: Default::default(),
        };
        let store: Arc<dyn BudgetStore> = Arc::new(InMemoryBudgetStore::new());
        let mut tracker = CognitiveBudgetTracker::new(config, store);

        // Consume way more than budget — should saturate, not overflow
        tracker.record_llm_call(LlmBackend::Local, u64::MAX / 2, u64::MAX / 2, 0.0);
        assert_eq!(tracker.remaining_local_tokens(), 0);
        // Should not panic
        tracker.record_llm_call(LlmBackend::Local, 1000, 1000, 0.0);
        assert_eq!(tracker.remaining_local_tokens(), 0);
    }

    #[test]
    fn build_consumption_reports_tick_tokens() {
        let mut tracker = test_tracker();
        tracker.record_llm_call(LlmBackend::Local, 500, 300, 0.0);
        let consumption = tracker.build_consumption();
        assert_eq!(consumption.len(), 1);
        assert_eq!(consumption[0].amount, 800);
    }
}
