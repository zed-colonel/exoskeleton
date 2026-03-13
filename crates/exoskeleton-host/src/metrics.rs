//! Prometheus metrics for the Exoskeleton Vessel.
//!
//! Uses a dedicated `prometheus::Registry` (not the global default) so multiple
//! Vessel instances in tests don't collide (H-1).

use exoskeleton_core::ExoError;
use prometheus::{
    CounterVec, Gauge, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts,
    Registry, TextEncoder,
};

/// Prometheus metrics for the Exoskeleton Vessel.
///
/// Uses a dedicated `prometheus::Registry` (not the global default) so multiple
/// Vessel instances in tests don't collide.
pub struct ExoMetrics {
    /// Dedicated registry for this vessel's metrics.
    pub registry: Registry,

    // ── Tick metrics ──
    /// Total ticks completed, by outcome label: "completed", "failed", "suspended".
    pub ticks_total: IntCounterVec,
    /// Tick duration in seconds (wall-clock time for the full PODAARA cycle).
    pub tick_duration_seconds: HistogramVec,

    // ── LLM metrics ──
    /// Total LLM calls, by backend: "local", "frontier".
    pub llm_calls_total: IntCounterVec,
    /// Total LLM tokens, by backend and direction: "input", "output".
    pub llm_tokens_total: IntCounterVec,
    /// Total LLM cost in cents, by backend.
    pub llm_cost_cents_total: CounterVec,
    /// LLM call latency in seconds, by backend.
    pub llm_latency_seconds: HistogramVec,

    // ── Thread metrics ──
    /// Total thread runs, by thread name and outcome: "completed", "failed", "skipped".
    pub thread_runs_total: IntCounterVec,

    // ── Action metrics (Act step → Tool AQ) ──
    /// Total actions executed, by connector name and outcome: "success", "failure", "rate_limited".
    pub actions_total: IntCounterVec,

    // ── Budget metrics ──
    /// Budget remaining, by dimension: "local_tokens", "frontier_tokens",
    /// "frontier_cost_cents", "tool_invocations".
    pub budget_remaining: GaugeVec,

    // ── Storage metrics ──
    /// Total relationship ledger entries.
    pub relationship_entries_total: IntGauge,
    /// Total artifacts stored, by kind.
    pub artifacts_total: IntCounterVec,

    // ── Engine metrics ──
    /// Current tick number (monotonically increasing).
    pub current_tick_number: IntGauge,
    /// Vessel uptime in seconds.
    pub uptime_seconds: Gauge,
    /// WI Host active flow count.
    pub tool_active_flows: IntGauge,
}

impl ExoMetrics {
    /// Create a new ExoMetrics with a dedicated prometheus registry.
    pub fn new() -> Result<Self, ExoError> {
        let registry = Registry::new();

        // Tick buckets tuned for PODAARA cycles
        let tick_buckets = vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0];
        // LLM latency buckets (longer tail for frontier calls)
        let llm_buckets = vec![0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0];

        let ticks_total = IntCounterVec::new(
            Opts::new("exo_ticks_total", "Total ticks completed"),
            &["outcome"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: ticks_total: {e}")))?;

        let tick_duration_seconds = HistogramVec::new(
            HistogramOpts::new("exo_tick_duration_seconds", "Tick duration in seconds")
                .buckets(tick_buckets),
            &["outcome"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: tick_duration_seconds: {e}")))?;

        let llm_calls_total = IntCounterVec::new(
            Opts::new("exo_llm_calls_total", "Total LLM calls"),
            &["backend"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: llm_calls_total: {e}")))?;

        let llm_tokens_total = IntCounterVec::new(
            Opts::new("exo_llm_tokens_total", "Total LLM tokens"),
            &["backend", "direction"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: llm_tokens_total: {e}")))?;

        let llm_cost_cents_total = CounterVec::new(
            Opts::new("exo_llm_cost_cents_total", "Total LLM cost in cents"),
            &["backend"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: llm_cost_cents_total: {e}")))?;

        let llm_latency_seconds = HistogramVec::new(
            HistogramOpts::new("exo_llm_latency_seconds", "LLM call latency in seconds")
                .buckets(llm_buckets),
            &["backend"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: llm_latency_seconds: {e}")))?;

        let thread_runs_total = IntCounterVec::new(
            Opts::new("exo_thread_runs_total", "Total thread runs"),
            &["thread_name", "outcome"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: thread_runs_total: {e}")))?;

        let actions_total = IntCounterVec::new(
            Opts::new("exo_actions_total", "Total actions executed"),
            &["connector", "outcome"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: actions_total: {e}")))?;

        let budget_remaining = GaugeVec::new(
            Opts::new("exo_budget_remaining", "Budget remaining by dimension"),
            &["dimension"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: budget_remaining: {e}")))?;

        let relationship_entries_total = IntGauge::new(
            "exo_relationship_entries_total",
            "Total relationship ledger entries",
        )
        .map_err(|e| ExoError::Config(format!("metrics: relationship_entries_total: {e}")))?;

        let artifacts_total = IntCounterVec::new(
            Opts::new("exo_artifacts_total", "Total artifacts stored"),
            &["kind"],
        )
        .map_err(|e| ExoError::Config(format!("metrics: artifacts_total: {e}")))?;

        let current_tick_number =
            IntGauge::new("exo_current_tick_number", "Current tick number")
                .map_err(|e| ExoError::Config(format!("metrics: current_tick_number: {e}")))?;

        let uptime_seconds = Gauge::new("exo_uptime_seconds", "Vessel uptime in seconds")
            .map_err(|e| ExoError::Config(format!("metrics: uptime_seconds: {e}")))?;

        let tool_active_flows = IntGauge::new("exo_tool_active_flows", "WI Host active flow count")
            .map_err(|e| ExoError::Config(format!("metrics: tool_active_flows: {e}")))?;

        // Register all metrics on the custom registry
        registry
            .register(Box::new(ticks_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(tick_duration_seconds.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(llm_calls_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(llm_tokens_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(llm_cost_cents_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(llm_latency_seconds.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(thread_runs_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(actions_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(budget_remaining.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(relationship_entries_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(artifacts_total.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(current_tick_number.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(uptime_seconds.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;
        registry
            .register(Box::new(tool_active_flows.clone()))
            .map_err(|e| ExoError::Config(format!("metrics register: {e}")))?;

        Ok(Self {
            registry,
            ticks_total,
            tick_duration_seconds,
            llm_calls_total,
            llm_tokens_total,
            llm_cost_cents_total,
            llm_latency_seconds,
            thread_runs_total,
            actions_total,
            budget_remaining,
            relationship_entries_total,
            artifacts_total,
            current_tick_number,
            uptime_seconds,
            tool_active_flows,
        })
    }

    /// Encode all metrics to Prometheus text format.
    pub fn render(&self) -> Result<String, ExoError> {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder
            .encode_to_string(&metric_families)
            .map_err(|e| ExoError::Config(format!("metrics render: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_succeeds_and_all_metrics_registered() {
        let m = ExoMetrics::new().unwrap();
        // Touch each metric to ensure they're all initialized
        m.ticks_total.with_label_values(&["completed"]).inc();
        m.tick_duration_seconds
            .with_label_values(&["completed"])
            .observe(0.0);
        m.llm_calls_total.with_label_values(&["local"]).inc();
        m.llm_tokens_total
            .with_label_values(&["local", "input"])
            .inc();
        m.llm_cost_cents_total.with_label_values(&["local"]).inc();
        m.llm_latency_seconds
            .with_label_values(&["local"])
            .observe(0.0);
        m.thread_runs_total
            .with_label_values(&["test", "completed"])
            .inc();
        m.actions_total
            .with_label_values(&["delay", "success"])
            .inc();
        m.budget_remaining
            .with_label_values(&["local_tokens"])
            .set(0.0);
        m.relationship_entries_total.set(0);
        m.artifacts_total.with_label_values(&["snapshot"]).inc();
        m.current_tick_number.set(0);
        m.uptime_seconds.set(0.0);
        m.tool_active_flows.set(0);

        let families = m.registry.gather();
        // Should have 14 metric families after touching all
        assert!(
            families.len() >= 14,
            "expected >= 14 metric families, got {}",
            families.len()
        );
    }

    #[test]
    fn counter_increment_and_read_back() {
        let m = ExoMetrics::new().unwrap();
        m.ticks_total.with_label_values(&["completed"]).inc();
        m.ticks_total.with_label_values(&["completed"]).inc();
        m.ticks_total.with_label_values(&["failed"]).inc();

        assert_eq!(m.ticks_total.with_label_values(&["completed"]).get(), 2);
        assert_eq!(m.ticks_total.with_label_values(&["failed"]).get(), 1);
        assert_eq!(m.ticks_total.with_label_values(&["suspended"]).get(), 0);
    }

    #[test]
    fn histogram_observation_and_render() {
        let m = ExoMetrics::new().unwrap();
        m.tick_duration_seconds
            .with_label_values(&["completed"])
            .observe(1.5);
        m.tick_duration_seconds
            .with_label_values(&["completed"])
            .observe(0.3);

        let rendered = m.render().unwrap();
        assert!(
            rendered.contains("exo_tick_duration_seconds"),
            "rendered metrics should contain tick_duration_seconds"
        );
    }

    #[test]
    fn label_value_combinations_work() {
        let m = ExoMetrics::new().unwrap();
        m.llm_tokens_total
            .with_label_values(&["local", "input"])
            .inc_by(100);
        m.llm_tokens_total
            .with_label_values(&["local", "output"])
            .inc_by(50);
        m.llm_tokens_total
            .with_label_values(&["frontier", "input"])
            .inc_by(200);

        assert_eq!(
            m.llm_tokens_total
                .with_label_values(&["local", "input"])
                .get(),
            100
        );
        assert_eq!(
            m.llm_tokens_total
                .with_label_values(&["local", "output"])
                .get(),
            50
        );
        assert_eq!(
            m.llm_tokens_total
                .with_label_values(&["frontier", "input"])
                .get(),
            200
        );
    }

    #[test]
    fn two_registries_dont_interfere() {
        let m1 = ExoMetrics::new().unwrap();
        let m2 = ExoMetrics::new().unwrap();

        m1.ticks_total.with_label_values(&["completed"]).inc();
        m1.ticks_total.with_label_values(&["completed"]).inc();

        // m2 should be at zero — independent registry
        assert_eq!(m2.ticks_total.with_label_values(&["completed"]).get(), 0);
        assert_eq!(m1.ticks_total.with_label_values(&["completed"]).get(), 2);
    }

    #[test]
    fn render_produces_valid_prometheus_text() {
        let m = ExoMetrics::new().unwrap();
        m.ticks_total.with_label_values(&["completed"]).inc();
        m.current_tick_number.set(42);

        let rendered = m.render().unwrap();
        assert!(rendered.contains("exo_ticks_total"));
        assert!(rendered.contains("exo_current_tick_number"));
        assert!(rendered.contains("42"));
    }

    #[test]
    fn zero_value_metrics_render_correctly() {
        let m = ExoMetrics::new().unwrap();
        // Don't increment anything — render should still work
        let rendered = m.render().unwrap();
        // Empty render should be valid (may contain HELP/TYPE lines but no sample lines
        // for untouched counters, or it may just be empty — both are valid)
        assert!(
            !rendered.contains("NaN"),
            "zero-value render should not contain NaN"
        );
    }

    #[test]
    fn gauge_set_and_read() {
        let m = ExoMetrics::new().unwrap();
        m.budget_remaining
            .with_label_values(&["local_tokens"])
            .set(1000.0);
        m.budget_remaining
            .with_label_values(&["frontier_tokens"])
            .set(500.0);

        assert_eq!(
            m.budget_remaining
                .with_label_values(&["local_tokens"])
                .get(),
            1000.0
        );
        assert_eq!(
            m.budget_remaining
                .with_label_values(&["frontier_tokens"])
                .get(),
            500.0
        );
    }
}
