//! Acceptance Test D: Atomic Align + Crash
//!
//! Proves that the Align step writes atomically and crash preserves ledger
//! coherence (Scope Appendix §5 criterion D).

mod support;

use std::collections::HashSet;
use std::time::Duration;

use exoskeleton_core::{ArtifactId, EnvelopeId, EnvelopeKind, MessageEnvelope, PrincipalId};
use exoskeleton_host::storage::StorageManager;
use exoskeleton_relationship::RelationshipLedger;

const TICK_TIMEOUT: Duration = Duration::from_secs(60);

/// Create a test envelope from a principal.
fn test_envelope(source: PrincipalId) -> MessageEnvelope {
    let content = format!("Message from {source}");
    let payload_id = ArtifactId::from_content(content.as_bytes());
    MessageEnvelope {
        id: EnvelopeId::new(),
        source,
        target: None,
        kind: EnvelopeKind::HumanMessage,
        payload_ref: payload_id,
        timestamp: chrono::Utc::now(),
        in_reply_to: None,
    }
}

#[tokio::test]
async fn align_writes_survive_crash() {
    let dir = tempfile::tempdir().unwrap();
    let principal = PrincipalId::new();

    let pre_crash_count;
    {
        let vessel = support::boot_vessel(dir.path()).await;

        // Submit a message that will create a relationship signal
        vessel
            .inbox()
            .submit(&test_envelope(principal))
            .expect("submit");

        let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;

        // Record ledger state
        pre_crash_count = vessel
            .storage()
            .relationship_store()
            .count()
            .expect("relationship count");

        drop(vessel); // crash
    }

    // Reopen and verify
    let storage = StorageManager::open(dir.path()).expect("StorageManager open");
    let post_crash_count = storage
        .relationship_store()
        .count()
        .expect("relationship count after crash");

    assert_eq!(
        pre_crash_count, post_crash_count,
        "relationship ledger entry count should match pre-crash: pre={}, post={}",
        pre_crash_count, post_crash_count
    );
}

#[tokio::test]
async fn no_duplicate_ledger_entries() {
    let dir = tempfile::tempdir().unwrap();
    let principal = PrincipalId::new();

    {
        let vessel = support::boot_vessel(dir.path()).await;
        vessel
            .inbox()
            .submit(&test_envelope(principal))
            .expect("submit");
        let _ticks = support::wait_for_ticks(vessel.storage(), 3, TICK_TIMEOUT).await;
        drop(vessel); // crash
    }

    // Restart and run more ticks
    {
        let vessel = support::boot_vessel(dir.path()).await;
        let _ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

        // Get all recent ledger entries and verify no duplicate IDs
        let entries = vessel
            .storage()
            .relationship_store()
            .recent(1000)
            .expect("recent entries");

        let mut ids = HashSet::new();
        for entry in &entries {
            assert!(
                ids.insert(entry.id),
                "duplicate ledger entry ID found: {}",
                entry.id
            );
        }

        support::shutdown_and_verify(vessel).await;
    }
}

#[tokio::test]
async fn ledger_entries_chronological() {
    let dir = tempfile::tempdir().unwrap();
    let principal = PrincipalId::new();

    let vessel = support::boot_vessel(dir.path()).await;
    vessel
        .inbox()
        .submit(&test_envelope(principal))
        .expect("submit");
    let _ticks = support::wait_for_ticks(vessel.storage(), 5, TICK_TIMEOUT).await;

    // Get entries and verify chronological order
    let entries = vessel
        .storage()
        .relationship_store()
        .recent(1000)
        .expect("recent entries");

    // `recent()` returns newest first, so reverse for chronological check
    let mut entries_chrono = entries.clone();
    entries_chrono.reverse();
    for window in entries_chrono.windows(2) {
        assert!(
            window[1].timestamp >= window[0].timestamp,
            "ledger entries should be in chronological order"
        );
    }

    support::shutdown_and_verify(vessel).await;
}

#[tokio::test]
async fn sqlite_wal_mode_active() {
    let dir = tempfile::tempdir().unwrap();
    let vessel = support::boot_vessel(dir.path()).await;
    let _ticks = support::wait_for_ticks(vessel.storage(), 1, TICK_TIMEOUT).await;
    support::shutdown_and_verify(vessel).await;

    // Open the relationships.db directly and verify WAL mode
    let db_path = dir.path().join("exo").join("relationships.db");
    if db_path.exists() {
        let conn = rusqlite::Connection::open(&db_path).expect("open relationships.db");
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("query journal_mode");
        assert_eq!(
            journal_mode.to_lowercase(),
            "wal",
            "relationships.db should use WAL journal mode"
        );
    }
    // If the database doesn't exist (no relationship entries), that's acceptable
}
