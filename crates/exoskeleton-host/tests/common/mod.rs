//! Shared test helpers for exoskeleton-host integration tests.

#![allow(dead_code)]

use std::num::NonZeroUsize;
use std::time::Duration;

use chrono::Utc;
use exoskeleton_core::llm::{
    ContentBlock, LlmBackend, LlmMessage, LlmRequest, LlmResponse, LlmRole, StopReason,
};
use exoskeleton_core::{ArtifactId, BudgetStatus, StateSnapshot, TickId, VesselId, VesselStatus};
use exoskeleton_host::config::{LlmConfig, VesselConfig};
use worldinterface_connector::connectors::default_registry;
use worldinterface_connector::registry::ConnectorRegistry;

/// Build a VesselConfig suitable for testing.
///
/// Uses aggressive timing: 10ms AQ tick intervals, 1-second master loop,
/// and short lease/shutdown timeouts. All tests use mock LLM backends.
pub fn test_config(dir: &std::path::Path) -> VesselConfig {
    VesselConfig {
        vessel_id: VesselId::new(),
        data_dir: dir.to_path_buf(),
        mission: "integration test".into(),
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
        max_decide_turns: 5,
        max_watches: 20,
        threads: None,
        source_repos: Vec::new(),
        sandbox: exoskeleton_host::config::SandboxConfig::default(),
        observatory_url: None,
        observatory_token_env: None,
        extra_destructive_tools: vec![],
        connectors_dir: None,
        coding_thread: exoskeleton_host::config::CodingThreadConfig::default(),
        tool_policy: exoskeleton_host::kernel::policy::ToolPolicyConfig::default(),
    }
}

/// Build a ConnectorRegistry with all built-in connectors.
///
/// Uses WorldInterface's `default_registry()` to stay in sync with upstream
/// connector additions (delay, http.request, fs.read, fs.write, code.read,
/// code.edit, code.write, code.grep, code.glob, code.ls, code.apply_patch,
/// shell.exec, sandbox.exec).
pub fn test_registry() -> ConnectorRegistry {
    default_registry()
}

pub fn make_snapshot(vessel_id: VesselId, tick_number: u64) -> StateSnapshot {
    StateSnapshot {
        vessel_id,
        tick_number,
        mission: "test mission".into(),
        plan: None,
        status: VesselStatus::Idle,
        vessel_mode: exoskeleton_core::VesselMode::Normal,
        working_memory: exoskeleton_core::working_memory::WorkingMemory::new(),
        thread_summaries: Vec::new(),
        exec_thread_summaries: Vec::new(),
        relationship_snapshot_ref: None,
        budget_status: BudgetStatus::unlimited(),
        last_action_summary: None,
        started_at: Some(Utc::now()),
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
        exec_thread_contributions: Vec::new(),
        actions_taken: Vec::new(),
        llm_calls: Vec::new(),
        decision_rationale: None,
        context_breakdown_ref: None,
    }
}

/// Helper to create a test LLM request.
pub fn test_llm_request() -> LlmRequest {
    LlmRequest {
        backend: None,
        system_prompt: Some("You are a test assistant.".into()),
        messages: vec![LlmMessage::text(LlmRole::User, "What is 2+2?")],
        max_output_tokens: 256,
        temperature: Some(0.0),
        stop_sequences: vec![],
        stream: false,
        tools: vec![],
    }
}

/// Build a mock LlmResponse, upgrading legacy JSON fixtures into content blocks.
///
/// Accepts either raw text (returned as a single Text block) or a JSON string
/// with `reasoning`, `actions`, and `memory_notes` fields. Actions become
/// ToolUse blocks; memory notes become `save_memory_note` ToolUse blocks.
pub fn mock_llm_response(content: &str) -> LlmResponse {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        let reasoning = value
            .get("reasoning")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let actions = value
            .get("actions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let memory_notes = value
            .get("memory_notes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut content_blocks = Vec::new();
        if !reasoning.is_empty() {
            content_blocks.push(ContentBlock::Text { text: reasoning });
        }
        for (idx, note) in memory_notes.iter().enumerate() {
            if let Some(note) = note.as_str() {
                content_blocks.push(ContentBlock::ToolUse {
                    id: format!("memory_note_{idx}"),
                    name: "save_memory_note".into(),
                    input: serde_json::json!({ "note": note }),
                });
            }
        }
        for (idx, action) in actions.iter().enumerate() {
            content_blocks.push(ContentBlock::ToolUse {
                id: format!("call_{idx}"),
                name: action["tool_name"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                input: action["params"].clone(),
            });
        }

        return LlmResponse {
            content_blocks,
            model: "mock-model".into(),
            tokens_in: 100,
            tokens_out: 50,
            latency_ms: 10,
            stop_reason: if actions.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            },
            cost_estimate_cents: None,
            backend: LlmBackend::Local,
        };
    }

    LlmResponse {
        content_blocks: vec![ContentBlock::Text {
            text: content.to_string(),
        }],
        model: "mock-model".into(),
        tokens_in: 100,
        tokens_out: 50,
        latency_ms: 10,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}

pub fn test_llm_response() -> LlmResponse {
    LlmResponse {
        content_blocks: vec![ContentBlock::Text {
            text: "The answer is 4.".into(),
        }],
        model: "mock-model".into(),
        tokens_in: 20,
        tokens_out: 8,
        latency_ms: 100,
        stop_reason: StopReason::EndTurn,
        cost_estimate_cents: None,
        backend: LlmBackend::Local,
    }
}
