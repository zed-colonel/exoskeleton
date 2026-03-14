//! Shared test helpers for exoskeleton-host integration tests.

#![allow(dead_code)]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmResponse, LlmRole, StopReason};
use exoskeleton_core::{ArtifactId, BudgetStatus, StateSnapshot, TickId, VesselId, VesselStatus};
use exoskeleton_host::config::{LlmConfig, VesselConfig};
use worldinterface_connector::connectors::{DelayConnector, FsReadConnector, FsWriteConnector};
use worldinterface_connector::registry::ConnectorRegistry;

/// Build a VesselConfig suitable for testing.
///
/// Uses fast tick intervals (10ms) and low concurrency (2) for speed.
pub fn test_config(dir: &std::path::Path) -> VesselConfig {
    VesselConfig {
        vessel_id: VesselId::new(),
        data_dir: dir.to_path_buf(),
        mission: "integration test".into(),
        cognitive_tick_interval: Duration::from_millis(10),
        cognitive_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        cognitive_lease_timeout_secs: 30,
        tool_tick_interval: Duration::from_millis(10),
        tool_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        shutdown_timeout: Duration::from_secs(5),
        llm_config: LlmConfig {
            timeout_secs: 10, // Must be < cognitive_lease_timeout_secs (30)
            ..LlmConfig::default()
        },
        master_loop_interval_secs: 10, // Must be < cognitive_lease_timeout_secs (30)
        inbox_dir: None,
        cognitive_budget: None,
        tool_budget: None,
        daemon_listen: None,
    }
}

/// Build a ConnectorRegistry WITHOUT the HTTP connector.
///
/// `HttpRequestConnector` creates an internal tokio runtime via
/// `reqwest::blocking::Client`, which conflicts with `#[tokio::test]`.
pub fn test_registry() -> ConnectorRegistry {
    let mut registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    registry.register(Arc::new(FsReadConnector));
    registry.register(Arc::new(FsWriteConnector));
    registry
}

pub fn make_snapshot(vessel_id: VesselId, tick_number: u64) -> StateSnapshot {
    StateSnapshot {
        vessel_id,
        tick_number,
        mission: "test mission".into(),
        plan: None,
        status: VesselStatus::Idle,
        working_context: String::new(),
        thread_summaries: Vec::new(),
        relationship_snapshot_ref: None,
        budget_status: BudgetStatus::unlimited(),
        last_action_summary: None,
        updated_at: Utc::now(),
    }
}

pub fn make_tick(tick_number: u64) -> exoskeleton_core::TickRecord {
    exoskeleton_core::TickRecord {
        tick_id: TickId::new(),
        tick_number,
        phase: exoskeleton_core::TickPhase::Amend,
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        snapshot_before: ArtifactId::from_content(format!("before-{tick_number}").as_bytes()),
        snapshot_after: Some(ArtifactId::from_content(
            format!("after-{tick_number}").as_bytes(),
        )),
        thread_contributions: Vec::new(),
        actions_taken: Vec::new(),
        llm_calls: Vec::new(),
        decision_rationale: None,
    }
}

/// Helper to create a test LLM request.
pub fn test_llm_request() -> LlmRequest {
    LlmRequest {
        backend: None,
        system_prompt: Some("You are a test assistant.".into()),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: "What is 2+2?".into(),
        }],
        max_output_tokens: 256,
        temperature: Some(0.0),
        stop_sequences: vec![],
    }
}

pub fn test_llm_response() -> LlmResponse {
    LlmResponse {
        content: "The answer is 4.".into(),
        model: "mock-model".into(),
        tokens_in: 20,
        tokens_out: 8,
        latency_ms: 100,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}
