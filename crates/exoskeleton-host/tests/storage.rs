//! Sprint 2: Persistence Layer integration tests.

mod common;

use chrono::Utc;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, ArtifactStore, EventEntry, EventLedger, EventType,
    LedgerEntryId, SnapshotStore, StateSnapshot, TickStore, VesselId,
};
use exoskeleton_host::storage::{
    SqliteArtifactStore, SqliteEventLedger, SqliteSnapshotStore, SqliteTickStore, StorageManager,
};
use exoskeleton_host::vessel::Vessel;

// ── T-6: Crash Simulation (File-Backed) ──

#[test]
fn artifact_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("artifacts.db");

    let artifact = Artifact::new(
        ArtifactKind::Snapshot,
        b"crash test artifact".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();

    {
        let store = SqliteArtifactStore::open(&db_path).unwrap();
        store.put(&artifact).unwrap();
        // Drop closes the connection
    }

    {
        let store = SqliteArtifactStore::open(&db_path).unwrap();
        let retrieved = store.get(&artifact_id).unwrap().unwrap();
        assert_eq!(retrieved.id, artifact_id);
        assert_eq!(retrieved.content, b"crash test artifact");
    }
}

#[test]
fn snapshot_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("snapshots.db");
    let vid = VesselId::new();

    {
        let store = SqliteSnapshotStore::open(&db_path).unwrap();
        store.save(&common::make_snapshot(vid, 0)).unwrap();
    }

    {
        let store = SqliteSnapshotStore::open(&db_path).unwrap();
        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_number, 0);
        assert_eq!(latest.vessel_id, vid);

        let at_tick = store.at_tick(0).unwrap().unwrap();
        assert_eq!(at_tick.tick_number, 0);
    }
}

#[test]
fn event_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("events.db");

    let entry = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "crash test event".into(),
        timestamp: Utc::now(),
    };
    let entry_id = entry.id;

    {
        let ledger = SqliteEventLedger::open(&db_path).unwrap();
        ledger.append(&entry).unwrap();
    }

    {
        let ledger = SqliteEventLedger::open(&db_path).unwrap();
        let recent = ledger.recent(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, entry_id);
    }
}

#[test]
fn tick_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("ticks.db");

    let record = common::make_tick(0);
    let tick_id = record.tick_id;

    {
        let store = SqliteTickStore::open(&db_path).unwrap();
        store.save(&record).unwrap();
    }

    {
        let store = SqliteTickStore::open(&db_path).unwrap();
        let retrieved = store.get(tick_id).unwrap().unwrap();
        assert_eq!(retrieved.tick_id, tick_id);

        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.tick_id, tick_id);
    }
}

#[test]
fn all_stores_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let vid = VesselId::new();

    let artifact = Artifact::new(
        ArtifactKind::Plan,
        b"survive all".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();
    let tick_record = common::make_tick(0);
    let tick_id = tick_record.tick_id;
    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "survive all event".into(),
        timestamp: Utc::now(),
    };
    let event_id = event.id;

    {
        let mgr = StorageManager::open(dir.path()).unwrap();
        mgr.artifact_store().put(&artifact).unwrap();
        mgr.snapshot_store()
            .save(&common::make_snapshot(vid, 0))
            .unwrap();
        mgr.event_ledger().append(&event).unwrap();
        mgr.tick_store().save(&tick_record).unwrap();
    }

    {
        let mgr = StorageManager::open(dir.path()).unwrap();
        assert!(mgr.artifact_store().exists(&artifact_id).unwrap());
        assert_eq!(
            mgr.snapshot_store().latest().unwrap().unwrap().tick_number,
            0
        );
        assert_eq!(mgr.event_ledger().recent(1).unwrap()[0].id, event_id);
        assert_eq!(
            mgr.tick_store().get(tick_id).unwrap().unwrap().tick_id,
            tick_id
        );
    }
}

// ── T-7: SQLite-Specific ──

#[test]
fn wal_mode_enabled() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    // Access the internal conn via put/get to verify WAL mode was set.
    // We verify by checking a known artifact.
    let artifact = Artifact::new(
        ArtifactKind::Snapshot,
        b"wal test".to_vec(),
        "text/plain".into(),
    );
    store.put(&artifact).unwrap();
    assert!(store.exists(&artifact.id).unwrap());
    // If pragmas were wrong, the store wouldn't function at all.
    // For file-backed stores, we can check more explicitly:
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteArtifactStore::open(&db_path).unwrap();

    // Check pragmas directly via a separate connection
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
}

#[test]
fn synchronous_full() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteSnapshotStore::open(&db_path).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 2, "synchronous should be FULL (2)");
}

#[test]
fn busy_timeout_set() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteEventLedger::open(&db_path).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let timeout: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    assert_eq!(timeout, 5000);
}

#[test]
fn strict_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let _store = SqliteArtifactStore::open(&db_path).unwrap();

    // STRICT tables reject type mismatches.
    // Try to insert a BLOB where TEXT is expected for 'kind'.
    // (SQLite STRICT coerces integers to TEXT, but rejects BLOBs in TEXT columns.)
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let result = conn.execute(
        "INSERT INTO artifacts (id, kind, content, content_type, created_at, metadata)
         VALUES ('abc', X'00', X'00', 'text/plain', '2024-01-01', '{}')",
        [],
    );
    assert!(
        result.is_err(),
        "STRICT table should reject BLOB in TEXT column"
    );
}

// ── T-8: Vessel Integration (Sprint 2) ──

#[tokio::test]
async fn vessel_start_creates_db_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let exo_dir = dir.path().join("exo");
    assert!(exo_dir.join("artifacts.db").exists());
    assert!(exo_dir.join("snapshots.db").exists());
    assert!(exo_dir.join("events.db").exists());
    assert!(exo_dir.join("ticks.db").exists());

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_storage_accessible() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let artifact = Artifact::new(
        ArtifactKind::Snapshot,
        b"vessel storage test".to_vec(),
        "text/plain".into(),
    );
    vessel.storage().artifact_store().put(&artifact).unwrap();
    let retrieved = vessel
        .storage()
        .artifact_store()
        .get(&artifact.id)
        .unwrap()
        .unwrap();
    assert_eq!(retrieved.content, b"vessel storage test");

    vessel.shutdown().await.unwrap();
}

#[tokio::test]
async fn vessel_storage_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());

    let artifact = Artifact::new(
        ArtifactKind::Plan,
        b"persist across restart".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();

    // First instance: write data
    {
        let vessel = Vessel::start_with_registry(config.clone(), common::test_registry())
            .await
            .unwrap();
        vessel.storage().artifact_store().put(&artifact).unwrap();
        vessel.shutdown().await.unwrap();
    }

    // Second instance: read data
    {
        let vessel = Vessel::start_with_registry(config, common::test_registry())
            .await
            .unwrap();
        let retrieved = vessel
            .storage()
            .artifact_store()
            .get(&artifact_id)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.content, b"persist across restart");
        vessel.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn vessel_shutdown_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vid = config.vessel_id;

    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    // Write to all stores
    let artifact = Artifact::new(
        ArtifactKind::Receipt,
        b"shutdown test".to_vec(),
        "text/plain".into(),
    );
    let artifact_id = artifact.id.clone();
    vessel.storage().artifact_store().put(&artifact).unwrap();

    let snap = common::make_snapshot(vid, 0);
    vessel.storage().snapshot_store().save(&snap).unwrap();

    let event = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "shutdown test event".into(),
        timestamp: Utc::now(),
    };
    vessel.storage().event_ledger().append(&event).unwrap();

    let tick_record = common::make_tick(0);
    let tick_id = tick_record.tick_id;
    vessel.storage().tick_store().save(&tick_record).unwrap();

    vessel.shutdown().await.unwrap();

    // Re-open stores directly (no Vessel) — data should be intact
    let mgr = StorageManager::open(dir.path()).unwrap();
    assert!(mgr.artifact_store().exists(&artifact_id).unwrap());
    assert_eq!(
        mgr.snapshot_store().latest().unwrap().unwrap().tick_number,
        0
    );
    assert_eq!(mgr.event_ledger().recent(1).unwrap().len(), 1);
    assert!(mgr.tick_store().get(tick_id).unwrap().is_some());
}

// ── T-9: Edge Cases ──

#[test]
fn large_artifact() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    let big_content = vec![0xABu8; 1024 * 1024]; // 1MB
    let artifact = Artifact::new(
        ArtifactKind::Memory,
        big_content.clone(),
        "application/octet-stream".into(),
    );

    store.put(&artifact).unwrap();
    let retrieved = store.get(&artifact.id).unwrap().unwrap();
    assert_eq!(retrieved.content.len(), 1024 * 1024);
    assert_eq!(retrieved.content, big_content);
}

#[test]
fn empty_artifact_content() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    let artifact = Artifact::new(ArtifactKind::Plan, Vec::new(), "text/plain".into());

    // ArtifactId is hash of empty bytes
    let expected_id = ArtifactId::from_content(&[]);
    assert_eq!(artifact.id, expected_id);

    store.put(&artifact).unwrap();
    let retrieved = store.get(&artifact.id).unwrap().unwrap();
    assert!(retrieved.content.is_empty());
}

#[test]
fn artifact_metadata_special_chars() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    let artifact = Artifact::new(
        ArtifactKind::Envelope,
        b"unicode metadata".to_vec(),
        "text/plain".into(),
    )
    .with_metadata("名前", "太郎")
    .with_metadata("emoji", "🦀🔥")
    .with_metadata("quotes", "He said \"hello\"");

    store.put(&artifact).unwrap();
    let retrieved = store.get(&artifact.id).unwrap().unwrap();
    assert_eq!(retrieved.metadata.get("名前"), Some(&"太郎".to_string()));
    assert_eq!(retrieved.metadata.get("emoji"), Some(&"🦀🔥".to_string()));
    assert_eq!(
        retrieved.metadata.get("quotes"),
        Some(&"He said \"hello\"".to_string())
    );
}

#[test]
fn event_summary_unicode() {
    let ledger = SqliteEventLedger::in_memory().unwrap();
    let entry = EventEntry {
        id: LedgerEntryId::new(),
        tick_id: None,
        event_type: EventType::VesselStarted,
        payload_ref: None,
        summary: "船が始まった 🚀 — vessel started".into(),
        timestamp: Utc::now(),
    };

    ledger.append(&entry).unwrap();
    let recent = ledger.recent(1).unwrap();
    assert_eq!(recent[0].summary, "船が始まった 🚀 — vessel started");
}

#[test]
fn many_artifacts_same_kind() {
    let store = SqliteArtifactStore::in_memory().unwrap();
    for i in 0..100 {
        let a = Artifact::new(
            ArtifactKind::Memory,
            format!("memory-{i}").into_bytes(),
            "text/plain".into(),
        );
        store.put(&a).unwrap();
    }

    let all = store.list_by_kind(ArtifactKind::Memory, 200).unwrap();
    assert_eq!(all.len(), 100);

    let limited = store.list_by_kind(ArtifactKind::Memory, 10).unwrap();
    assert_eq!(limited.len(), 10);
}

// ── T-10: Property-Based Tests ──

mod proptest_tests {
    use exoskeleton_core::BudgetStatus;
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn any_artifact_roundtrips(
            content in proptest::collection::vec(any::<u8>(), 0..512),
            kind_idx in 0usize..9,
        ) {
            let kinds = [
                ArtifactKind::Snapshot,
                ArtifactKind::Plan,
                ArtifactKind::Receipt,
                ArtifactKind::ThreadOutput,
                ArtifactKind::RelationshipEntry,
                ArtifactKind::Memory,
                ArtifactKind::Decision,
                ArtifactKind::Envelope,
                ArtifactKind::LlmResponse,
            ];
            let kind = kinds[kind_idx];
            let artifact = Artifact::new(kind, content, "application/octet-stream".into());

            let store = SqliteArtifactStore::in_memory().unwrap();
            store.put(&artifact).unwrap();
            let retrieved = store.get(&artifact.id).unwrap().unwrap();
            prop_assert_eq!(artifact.id, retrieved.id);
            prop_assert_eq!(artifact.kind, retrieved.kind);
            prop_assert_eq!(artifact.content, retrieved.content);
        }

        #[test]
        fn artifact_id_is_deterministic_through_store(
            ref content in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let a = Artifact::new(
                ArtifactKind::Plan,
                content.clone(),
                "text/plain".into(),
            );
            let b = Artifact::new(
                ArtifactKind::Plan,
                content.clone(),
                "text/plain".into(),
            );

            let store = SqliteArtifactStore::in_memory().unwrap();
            let id1 = store.put(&a).unwrap();
            let id2 = store.put(&b).unwrap();
            prop_assert_eq!(id1, id2);
        }

        #[test]
        fn snapshot_roundtrips_through_store(
            tick_number in 0u64..10_000,
            status_idx in 0usize..6,
        ) {
            let statuses = [
                exoskeleton_core::VesselStatus::Idle,
                exoskeleton_core::VesselStatus::Thinking,
                exoskeleton_core::VesselStatus::Acting,
                exoskeleton_core::VesselStatus::Reflecting,
                exoskeleton_core::VesselStatus::Suspended,
                exoskeleton_core::VesselStatus::Shutdown,
            ];
            let status = statuses[status_idx];

            let snap = StateSnapshot {
                vessel_id: VesselId::new(),
                tick_number,
                mission: "proptest mission".into(),
                plan: None,
                status,
                working_context: String::new(),
                thread_summaries: Vec::new(),
                relationship_snapshot_ref: None,
                budget_status: BudgetStatus::unlimited(),
                last_action_summary: None,
                updated_at: Utc::now(),
            };

            let store = SqliteSnapshotStore::in_memory().unwrap();
            store.save(&snap).unwrap();
            let retrieved = store.at_tick(tick_number).unwrap().unwrap();
            prop_assert_eq!(snap, retrieved);
        }

        #[test]
        fn event_type_roundtrips_through_store(
            type_idx in 0usize..10,
        ) {
            let types = [
                EventType::VesselStarted,
                EventType::VesselStopped,
                EventType::TickStarted,
                EventType::TickCompleted,
                EventType::ActionExecuted,
                EventType::LlmCalled,
                EventType::ThreadRan,
                EventType::RelationshipUpdated,
                EventType::BudgetConsumed,
                EventType::Error,
            ];
            let event_type = types[type_idx];

            let entry = EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type,
                payload_ref: None,
                summary: format!("{event_type:?}"),
                timestamp: Utc::now(),
            };

            let ledger = SqliteEventLedger::in_memory().unwrap();
            ledger.append(&entry).unwrap();
            let recent = ledger.recent(1).unwrap();
            prop_assert_eq!(recent[0].event_type, event_type);
        }
    }
}
