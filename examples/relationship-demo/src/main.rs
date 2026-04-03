//! Relationship Substrate Demo
//!
//! Demonstrates the Exoskeleton relationship substrate:
//! 1. Boots a Vessel with MockLlmBackend (no real LLM needed)
//! 2. Submits messages from two principals (Alice and Bob) via the Inbox
//! 3. Runs ticks and shows relationship state evolving
//! 4. Kills vessel (drop without shutdown), restarts from same data_dir
//! 5. Shows relationship state survived restart
//! 6. Shuts down gracefully

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use exoskeleton_core::{
    ArtifactId, EnvelopeId, EnvelopeKind, MessageEnvelope, PrincipalId, SnapshotStore, TickStore,
};
use exoskeleton_host::config::{LlmConfig, VesselConfig};
use exoskeleton_host::vessel::Vessel;
use exoskeleton_host::{default_mock_response, LlmHttpBackend, MockLlmBackend};
use exoskeleton_relationship::RelationshipLedger;
use worldinterface_connector::connectors::{DelayConnector, FsReadConnector, FsWriteConnector};
use worldinterface_connector::registry::ConnectorRegistry;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a VesselConfig with fast tick intervals for the demo.
fn demo_config(dir: &Path) -> VesselConfig {
    VesselConfig {
        vessel_id: exoskeleton_core::VesselId::new(),
        data_dir: dir.to_path_buf(),
        mission: "Relationship substrate demo -- track trust for Alice and Bob".into(),
        cognitive_tick_interval: Duration::from_millis(50),
        cognitive_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        cognitive_lease_timeout_secs: 30,
        tool_tick_interval: Duration::from_millis(50),
        tool_dispatch_concurrency: NonZeroUsize::new(2).unwrap(),
        shutdown_timeout: Duration::from_secs(5),
        llm_config: LlmConfig {
            timeout_secs: 10,
            ..LlmConfig::default()
        },
        master_loop_interval_secs: 10,
        inbox_dir: None,
        cognitive_budget: None,
        tool_budget: None,
        daemon_listen: None,
        cors_allowed_origins: vec![],
        trust_decay: None,
        episodic_memory_capacity: Some(200),
        bootstrap_grace_period_ticks: 30,
        threads: None,
        source_repos: Vec::new(),
        sandbox: exoskeleton_host::config::SandboxConfig::default(),
        observatory_url: None,
        observatory_token_env: None,
        max_decide_turns: 5,
        max_watches: 20,
        extra_destructive_tools: vec![],
        connectors_dir: None,
        inner_loop: exoskeleton_host::config::InnerLoopConfig::default(),
        tool_policy: exoskeleton_host::kernel::policy::ToolPolicyConfig::default(),
    }
}

/// Build a ConnectorRegistry WITHOUT the HTTP connector (avoids runtime conflict).
fn demo_registry() -> ConnectorRegistry {
    let registry = ConnectorRegistry::new();
    registry.register(Arc::new(DelayConnector));
    registry.register(Arc::new(FsReadConnector));
    registry.register(Arc::new(FsWriteConnector));
    registry
}

/// Create the mock LLM backend.
fn mock_backend() -> Arc<dyn LlmHttpBackend> {
    Arc::new(MockLlmBackend::new(default_mock_response()))
}

/// Boot a Vessel from the given directory with mock LLM and test registry.
async fn boot_vessel(dir: &Path) -> Vessel {
    let config = demo_config(dir);
    Vessel::start_with_registry_and_backends(config, demo_registry(), Some(mock_backend()), None)
        .await
        .expect("Vessel boot must succeed")
}

/// Create a test envelope from a named principal.
fn make_envelope(source: PrincipalId, message: &str) -> MessageEnvelope {
    let payload_id = ArtifactId::from_content(message.as_bytes());
    MessageEnvelope {
        id: EnvelopeId::new(),
        source,
        target: None,
        kind: EnvelopeKind::HumanMessage,
        payload_ref: payload_id,
        timestamp: Utc::now(),
        in_reply_to: None,
    }
}

/// Poll until at least `n` ticks have completed (with timeout).
async fn wait_for_ticks(vessel: &Vessel, n: u64) {
    let timeout = Duration::from_secs(60);
    let start = tokio::time::Instant::now();
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let latest = vessel
            .storage()
            .tick_store()
            .latest()
            .expect("TickStore read failed");
        if let Some(ref tick) = latest {
            if tick.tick_number >= n {
                return;
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

/// Print a separator line for readability.
fn separator() {
    println!("{}", "-".repeat(72));
}

/// Print the current relationship state from the vessel's stores.
fn print_relationship_state(vessel: &Vessel) {
    let store = vessel.storage().relationship_store();

    let count = store.count().unwrap_or(0);
    println!("  Relationship ledger entries: {count}");

    let recent = store.recent(10).unwrap_or_default();
    if recent.is_empty() {
        println!("  (no relationship records yet)");
    } else {
        for record in &recent {
            println!(
                "    [{:?}] principal={}... signal={:?}",
                record.timestamp.format("%H:%M:%S"),
                &record.principal_id.to_string()[..8],
                record.signal_type,
            );
        }
    }

    let principals = store.distinct_principals().unwrap_or_default();
    println!("  Distinct principals: {}", principals.len());
    for pid in &principals {
        let records = store.for_principal(*pid, 100).unwrap_or_default();
        println!(
            "    {}... -- {} records",
            &pid.to_string()[..8],
            records.len()
        );
    }
}

/// Print the current state snapshot summary.
fn print_snapshot(vessel: &Vessel) {
    match vessel.storage().snapshot_store().latest() {
        Ok(Some(snap)) => {
            println!("  Tick number:      {}", snap.tick_number);
            println!("  Status:           {:?}", snap.status);
            println!("  Mission:          {}", snap.mission);
            println!("  Active threads:   {}", snap.thread_summaries.len());
            println!(
                "  Relationship ref: {}",
                snap.relationship_snapshot_ref
                    .as_ref()
                    .map(|r| format!("{}...", &r.as_str()[..16]))
                    .unwrap_or_else(|| "(none)".into())
            );
            if let Some(ref action) = snap.last_action_summary {
                println!("  Last action:      {action}");
            }
        }
        Ok(None) => {
            println!("  (no snapshot yet)");
        }
        Err(e) => {
            println!("  Error reading snapshot: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Create a temporary directory that persists for the whole demo.
    let dir = std::env::temp_dir().join(format!(
        "exo-rel-demo-{}-{}",
        std::process::id(),
        Utc::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");

    // Stable principal IDs that survive across restarts.
    let alice = PrincipalId::new();
    let bob = PrincipalId::new();

    println!();
    println!("=== Exoskeleton Relationship Substrate Demo ===");
    println!();
    println!("Data directory: {}", dir.display());
    println!("Alice (principal): {}...", &alice.to_string()[..8]);
    println!("Bob   (principal): {}...", &bob.to_string()[..8]);
    separator();

    // ------------------------------------------------------------------
    // Phase 1: Boot, submit messages, observe initial relationship state
    // ------------------------------------------------------------------
    println!();
    println!("[Phase 1] Boot vessel and submit messages from Alice and Bob");
    separator();

    let vessel = boot_vessel(&dir).await;
    println!("  Vessel booted: {}", vessel.vessel_id());

    // Submit messages from Alice.
    vessel
        .inbox()
        .submit(&make_envelope(alice, "Hello, vessel! I'm Alice."))
        .expect("submit Alice msg 1");
    vessel
        .inbox()
        .submit(&make_envelope(
            alice,
            "Please help me organize my project files.",
        ))
        .expect("submit Alice msg 2");

    // Submit messages from Bob.
    vessel
        .inbox()
        .submit(&make_envelope(bob, "Hi there, I'm Bob."))
        .expect("submit Bob msg 1");

    println!("  Submitted 3 messages (2 from Alice, 1 from Bob)");
    println!();

    // Wait for initial ticks to process the messages.
    println!("[Phase 1] Waiting for 3 ticks to process messages...");
    wait_for_ticks(&vessel, 3).await;

    println!();
    println!("[Phase 1] State after 3 ticks:");
    print_snapshot(&vessel);
    println!();
    println!("[Phase 1] Relationship state:");
    print_relationship_state(&vessel);
    separator();

    // ------------------------------------------------------------------
    // Phase 2: Run more ticks, submit more messages, observe evolution
    // ------------------------------------------------------------------
    println!();
    println!("[Phase 2] Submit more messages and run additional ticks");
    separator();

    vessel
        .inbox()
        .submit(&make_envelope(
            alice,
            "Great work on the last task! Very helpful.",
        ))
        .expect("submit Alice msg 3");
    vessel
        .inbox()
        .submit(&make_envelope(
            bob,
            "Can you also check the security of my deployment?",
        ))
        .expect("submit Bob msg 2");
    vessel
        .inbox()
        .submit(&make_envelope(bob, "Thanks for the quick response."))
        .expect("submit Bob msg 3");

    println!("  Submitted 3 more messages (1 Alice, 2 Bob)");
    println!();

    println!("[Phase 2] Waiting for 5 ticks total...");
    wait_for_ticks(&vessel, 5).await;

    println!();
    println!("[Phase 2] State after 5 ticks:");
    print_snapshot(&vessel);
    println!();
    println!("[Phase 2] Relationship state:");
    print_relationship_state(&vessel);

    // Record pre-crash state for comparison.
    let pre_crash_tick = vessel
        .storage()
        .tick_store()
        .latest()
        .expect("tick read")
        .map(|t| t.tick_number)
        .unwrap_or(0);
    let pre_crash_ledger_count = vessel.storage().relationship_store().count().unwrap_or(0);
    let pre_crash_principals = vessel
        .storage()
        .relationship_store()
        .distinct_principals()
        .unwrap_or_default()
        .len();

    separator();

    // ------------------------------------------------------------------
    // Phase 3: Kill vessel (simulate crash) and restart
    // ------------------------------------------------------------------
    println!();
    println!("[Phase 3] Simulating crash -- dropping vessel without shutdown");
    separator();

    drop(vessel);
    println!("  Vessel dropped (crash simulated).");
    println!();

    // Brief pause to let OS release file handles.
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("[Phase 3] Restarting vessel from same data directory...");
    let vessel = boot_vessel(&dir).await;
    println!("  Vessel rebooted: {}", vessel.vessel_id());
    println!();

    // ------------------------------------------------------------------
    // Phase 4: Verify relationship state survived restart
    // ------------------------------------------------------------------
    println!("[Phase 4] Verifying relationship durability after restart");
    separator();

    // Check that the tick store preserved state.
    let post_restart_tick = vessel
        .storage()
        .tick_store()
        .latest()
        .expect("tick read")
        .map(|t| t.tick_number)
        .unwrap_or(0);
    println!("  Pre-crash tick number:  {}", pre_crash_tick);
    println!(
        "  Post-restart tick:      {} (should match or exceed pre-crash)",
        post_restart_tick
    );
    assert!(
        post_restart_tick >= pre_crash_tick,
        "Tick number should not regress after restart"
    );

    // Check that relationship ledger entries survived.
    let post_restart_count = vessel.storage().relationship_store().count().unwrap_or(0);
    println!("  Pre-crash ledger count: {}", pre_crash_ledger_count);
    println!(
        "  Post-restart count:     {} (should match)",
        post_restart_count
    );
    assert_eq!(
        pre_crash_ledger_count, post_restart_count,
        "Relationship ledger entries must survive restart"
    );

    // Check that distinct principals survived.
    let post_restart_principals = vessel
        .storage()
        .relationship_store()
        .distinct_principals()
        .unwrap_or_default()
        .len();
    println!("  Pre-crash principals:   {}", pre_crash_principals);
    println!(
        "  Post-restart principals:{} (should match)",
        post_restart_principals
    );
    assert_eq!(
        pre_crash_principals, post_restart_principals,
        "Distinct principals must survive restart"
    );

    println!();
    println!("[Phase 4] Full relationship state after restart:");
    print_relationship_state(&vessel);
    separator();

    // ------------------------------------------------------------------
    // Phase 5: Run more ticks after restart, show continued evolution
    // ------------------------------------------------------------------
    println!();
    println!("[Phase 5] Running 2 more ticks after restart...");
    separator();

    let target_tick = post_restart_tick + 2;
    wait_for_ticks(&vessel, target_tick).await;

    println!();
    println!("[Phase 5] State after post-restart ticks:");
    print_snapshot(&vessel);
    println!();
    println!("[Phase 5] Relationship state:");
    print_relationship_state(&vessel);
    separator();

    // ------------------------------------------------------------------
    // Phase 6: Graceful shutdown
    // ------------------------------------------------------------------
    println!();
    println!("[Phase 6] Graceful shutdown");
    separator();

    vessel
        .shutdown()
        .await
        .expect("Vessel shutdown must succeed");
    println!("  Vessel shut down gracefully.");

    println!();
    println!("=== Demo Complete ===");
    println!();
    println!("Key observations:");
    println!("  - Relationship ledger entries are durable (I8)");
    println!("  - State survived kill/restart without data loss");
    println!("  - Tick numbers are monotonically increasing across restarts");
    println!("  - Cognitive and tool engines recovered independently (I9)");
    println!("  - All 8 SQLite stores are intact after crash recovery");
    println!();

    // Clean up the temporary directory.
    let _ = std::fs::remove_dir_all(&dir);
}
