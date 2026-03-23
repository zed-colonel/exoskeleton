//! Shared test harness for acceptance tests.
//!
//! Provides Vessel boot helpers, mock LLM, tick-waiting utilities, and
//! registry construction. All acceptance tests share these utilities.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use exoskeleton_core::VesselId;
use exoskeleton_core::{TickRecord, TickStore};
use exoskeleton_host::config::{LlmConfig, VesselConfig};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_host::vessel::Vessel;
use exoskeleton_host::{default_mock_response, LlmHttpBackend, MockLlmBackend};
use worldinterface_connector::connectors::{
    DelayConnector, FsReadConnector, FsWriteConnector, HttpRequestConnector,
};
use worldinterface_connector::registry::ConnectorRegistry;

/// Build a VesselConfig suitable for acceptance testing.
///
/// Uses aggressive timing: 10ms AQ tick intervals, 1-second master loop,
/// and short lease/shutdown timeouts. All tests use mock LLM backends,
/// so real network latency is not a factor.
pub fn test_config(dir: &Path) -> VesselConfig {
    VesselConfig {
        vessel_id: VesselId::new(),
        data_dir: dir.to_path_buf(),
        mission: "acceptance test".into(),
        cognitive_tick_interval: Duration::from_millis(10),
        cognitive_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        cognitive_lease_timeout_secs: 5,
        tool_tick_interval: Duration::from_millis(10),
        tool_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        shutdown_timeout: Duration::from_secs(2),
        llm_config: LlmConfig {
            timeout_secs: 2,
            ..LlmConfig::default()
        },
        master_loop_interval_secs: 1,
        inbox_dir: None,
        cognitive_budget: None,
        tool_budget: None,
        daemon_listen: None,
        cors_allowed_origins: vec![],
        trust_decay: None,
        episodic_memory_capacity: Some(200),
        bootstrap_grace_period_ticks: 0,
        threads: None,
        source_repos: Vec::new(),
        sandbox: exoskeleton_host::config::SandboxConfig::default(),
        observatory_url: None,
        observatory_token_env: None,
    }
}

/// Build a ConnectorRegistry with all built-in connectors including HTTP.
pub fn test_registry() -> ConnectorRegistry {
    let mut registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    registry.register(Arc::new(FsReadConnector));
    registry.register(Arc::new(FsWriteConnector));
    registry.register(Arc::new(HttpRequestConnector::new()));
    registry
}

/// Create the default mock LLM backend (deterministic responses).
pub fn mock_backend() -> Arc<dyn LlmHttpBackend> {
    Arc::new(MockLlmBackend::new(default_mock_response()))
}

/// Boot a Vessel with mock LLM backends and the standard test registry.
pub async fn boot_vessel(dir: &Path) -> Vessel {
    let config = test_config(dir);
    Vessel::start_with_registry_and_backends(config, test_registry(), Some(mock_backend()), None)
        .await
        .expect("Vessel boot must succeed")
}

/// Boot a Vessel with a custom config, mock LLM, and the standard test registry.
#[allow(dead_code)]
pub async fn boot_vessel_with_config(config: VesselConfig) -> Vessel {
    Vessel::start_with_registry_and_backends(config, test_registry(), Some(mock_backend()), None)
        .await
        .expect("Vessel boot must succeed")
}

/// Build a VesselConfig with optional budget configurations.
///
/// Uses the same fast tick intervals as `test_config()`, but applies the
/// given cognitive and/or tool budget configs.
#[allow(dead_code)]
pub fn test_config_with_budget(
    dir: &Path,
    cognitive_budget: Option<exoskeleton_core::CognitiveBudgetConfig>,
    tool_budget: Option<exoskeleton_core::ToolBudgetConfig>,
) -> VesselConfig {
    let mut config = test_config(dir);
    config.cognitive_budget = cognitive_budget;
    config.tool_budget = tool_budget;
    config
}

/// Poll the TickStore until `n` ticks have completed, or panic on timeout.
///
/// Uses a 50ms poll interval with a configurable timeout. Returns the
/// collected tick records ordered by tick_number.
pub async fn wait_for_ticks(
    storage: &StorageManager,
    n: u64,
    timeout: Duration,
) -> Vec<TickRecord> {
    let start = tokio::time::Instant::now();
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    loop {
        interval.tick().await;
        let latest = storage
            .tick_store()
            .latest()
            .expect("TickStore read failed");
        if let Some(ref tick) = latest {
            if tick.tick_number >= n {
                // Fetch all ticks from 1..=n
                let ticks = storage
                    .tick_store()
                    .range(1, n)
                    .expect("TickStore range failed");
                return ticks;
            }
        }
        if start.elapsed() > timeout {
            let got = latest.map(|t| t.tick_number).unwrap_or(0);
            panic!(
                "Timeout waiting for {} ticks (got {} in {:?})",
                n, got, timeout
            );
        }
    }
}

/// Gracefully shut down a Vessel and assert no panics occurred.
pub async fn shutdown_and_verify(vessel: Vessel) {
    vessel
        .shutdown()
        .await
        .expect("Vessel shutdown must succeed");
}
