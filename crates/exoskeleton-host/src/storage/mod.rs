//! Persistence layer: SQLite-backed stores for artifacts, snapshots, events, ticks, memory,
//! threads, relationships, budget, conversations.
//!
//! All meaningful state in Exoskeleton is stored through these nine stores.
//! Together with the ActionQueue WALs (Cognitive AQ + Tool AQ), they provide
//! the complete replay substrate required by I3.
//!
//! Each store has its own SQLite database file under `{data_dir}/exo/`:
//! - `artifacts.db` — content-addressed artifact store
//! - `snapshots.db` — state snapshot history
//! - `events.db` — append-only event ledger
//! - `ticks.db` — completed tick records
//! - `memory.db` — episodic summaries + long-term notes (Sprint 3)
//! - `threads.db` — thread specs + outputs (Sprint 6)
//! - `relationships.db` — append-only relationship ledger (Sprint 8)
//! - `budget.db` — budget window state (Sprint 9)
//! - `conversations.db` — conversation grouping + state (E1-S2)
//!
//! Separate files avoid WAL contention between tables and simplify backup/restore.
//! Exception: `memory.db` contains two tables (episodic + long-term) since they
//! are rarely written concurrently and don't benefit from separate WALs.

pub mod artifact_store;
pub mod budget_store;
pub mod conversation_store;
pub mod event_ledger;
pub mod memory_store;
pub mod relationship_store;
pub mod snapshot_store;
pub mod thread_store;
pub mod tick_store;
pub mod watch_store;

use std::path::Path;
use std::sync::Arc;

pub use artifact_store::SqliteArtifactStore;
pub use budget_store::SqliteBudgetStore;
pub use conversation_store::SqliteConversationStore;
pub use event_ledger::SqliteEventLedger;
use exoskeleton_core::ExoError;
pub use memory_store::SqliteMemoryStore;
pub use relationship_store::SqliteRelationshipLedger;
pub use snapshot_store::SqliteSnapshotStore;
pub use thread_store::SqliteThreadStore;
pub use tick_store::SqliteTickStore;
pub use watch_store::SqliteWatchStore;

/// Owns all persistence stores and provides access to them.
///
/// All SQLite databases are stored under `{data_dir}/exo/`:
/// - `{data_dir}/exo/artifacts.db` — content-addressed artifact store
/// - `{data_dir}/exo/snapshots.db` — state snapshot history
/// - `{data_dir}/exo/events.db` — append-only event ledger
/// - `{data_dir}/exo/ticks.db` — completed tick records
/// - `{data_dir}/exo/memory.db` — memory tiers: episodic + long-term (Sprint 3)
/// - `{data_dir}/exo/threads.db` — thread specs + outputs (Sprint 6)
/// - `{data_dir}/exo/relationships.db` — append-only relationship ledger (Sprint 8)
/// - `{data_dir}/exo/budget.db` — budget window state (Sprint 9)
/// - `{data_dir}/exo/conversations.db` — conversation grouping + state (E1-S2)
///
/// Each store has its own database file (not one monolithic DB) for:
/// - Independent WAL performance (no cross-table WAL contention)
/// - Simpler backup/restore (copy the file you need)
/// - Clear separation of concerns
#[derive(Clone)]
pub struct StorageManager {
    artifact_store: Arc<SqliteArtifactStore>,
    snapshot_store: Arc<SqliteSnapshotStore>,
    event_ledger: Arc<SqliteEventLedger>,
    tick_store: Arc<SqliteTickStore>,
    memory_store: Arc<SqliteMemoryStore>,
    thread_store: Arc<SqliteThreadStore>,
    relationship_store: Arc<SqliteRelationshipLedger>,
    budget_store: Arc<SqliteBudgetStore>,
    conversation_store: Arc<SqliteConversationStore>,
    watch_store: Arc<SqliteWatchStore>,
}

impl StorageManager {
    /// Create fresh, empty stores in a new data directory.
    ///
    /// Like `open()`, but validates that no stores already exist. Returns
    /// `ExoError::Storage` if the `{data_dir}/exo/` directory already exists.
    /// This prevents accidentally overwriting an existing vessel's data.
    pub fn create_fresh(data_dir: &Path) -> Result<Self, ExoError> {
        let exo_dir = data_dir.join("exo");
        if exo_dir.exists() {
            return Err(ExoError::Storage(format!(
                "data directory already has stores: {}",
                exo_dir.display()
            )));
        }
        Self::open(data_dir)
    }

    /// Open or create all stores under the given data directory.
    ///
    /// Creates `{data_dir}/exo/` if it doesn't exist (should already exist
    /// from `ensure_data_dirs` in Sprint 1).
    pub fn open(data_dir: &Path) -> Result<Self, ExoError> {
        let exo_dir = data_dir.join("exo");
        std::fs::create_dir_all(&exo_dir).map_err(|e| {
            ExoError::Storage(format!(
                "failed to create exo dir {}: {e}",
                exo_dir.display()
            ))
        })?;

        let artifact_store = Arc::new(SqliteArtifactStore::open(exo_dir.join("artifacts.db"))?);
        let snapshot_store = Arc::new(SqliteSnapshotStore::open(exo_dir.join("snapshots.db"))?);
        let event_ledger = Arc::new(SqliteEventLedger::open(exo_dir.join("events.db"))?);
        let tick_store = Arc::new(SqliteTickStore::open(exo_dir.join("ticks.db"))?);
        let memory_store = Arc::new(SqliteMemoryStore::open(exo_dir.join("memory.db"))?);
        let thread_store = Arc::new(SqliteThreadStore::open(exo_dir.join("threads.db"))?);
        let relationship_store = Arc::new(SqliteRelationshipLedger::open(
            exo_dir.join("relationships.db"),
        )?);
        let budget_store = Arc::new(SqliteBudgetStore::open(exo_dir.join("budget.db"))?);
        let conversation_store = Arc::new(SqliteConversationStore::open(
            exo_dir.join("conversations.db"),
        )?);
        let watch_store = Arc::new(SqliteWatchStore::open(exo_dir.join("watches.db"))?);

        Ok(Self {
            artifact_store,
            snapshot_store,
            event_ledger,
            tick_store,
            memory_store,
            thread_store,
            relationship_store,
            budget_store,
            conversation_store,
            watch_store,
        })
    }

    /// Access the artifact store.
    pub fn artifact_store(&self) -> &Arc<SqliteArtifactStore> {
        &self.artifact_store
    }

    /// Access the snapshot store.
    pub fn snapshot_store(&self) -> &Arc<SqliteSnapshotStore> {
        &self.snapshot_store
    }

    /// Access the event ledger.
    pub fn event_ledger(&self) -> &Arc<SqliteEventLedger> {
        &self.event_ledger
    }

    /// Access the tick store.
    pub fn tick_store(&self) -> &Arc<SqliteTickStore> {
        &self.tick_store
    }

    /// Access the memory store (Sprint 3).
    pub fn memory_store(&self) -> &Arc<SqliteMemoryStore> {
        &self.memory_store
    }

    /// Access the thread store (Sprint 6).
    pub fn thread_store(&self) -> &Arc<SqliteThreadStore> {
        &self.thread_store
    }

    /// Access the relationship store (Sprint 8).
    pub fn relationship_store(&self) -> &Arc<SqliteRelationshipLedger> {
        &self.relationship_store
    }

    /// Access the budget store (Sprint 9).
    pub fn budget_store(&self) -> &Arc<SqliteBudgetStore> {
        &self.budget_store
    }

    /// Access the conversation store (E1-S2).
    pub fn conversation_store(&self) -> &Arc<SqliteConversationStore> {
        &self.conversation_store
    }

    /// Access the watch store (E5-S2).
    pub fn watch_store(&self) -> &Arc<SqliteWatchStore> {
        &self.watch_store
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        Artifact, ArtifactId, ArtifactKind, ArtifactStore, EpisodicSummary, EventLedger,
        LongTermNote, SnapshotStore, StateSnapshot, TickStore, VesselId,
    };
    use exoskeleton_memory::MemoryStore;

    use super::*;

    // ── T-5: Storage Manager ──

    #[test]
    fn open_creates_all_db_files() {
        let dir = tempfile::tempdir().unwrap();
        let _mgr = StorageManager::open(dir.path()).unwrap();

        let exo_dir = dir.path().join("exo");
        assert!(exo_dir.join("artifacts.db").exists());
        assert!(exo_dir.join("snapshots.db").exists());
        assert!(exo_dir.join("events.db").exists());
        assert!(exo_dir.join("ticks.db").exists());
        assert!(exo_dir.join("memory.db").exists()); // Sprint 3
        assert!(exo_dir.join("threads.db").exists()); // Sprint 6
        assert!(exo_dir.join("relationships.db").exists()); // Sprint 8
        assert!(exo_dir.join("budget.db").exists()); // Sprint 9
        assert!(exo_dir.join("conversations.db").exists()); // E1-S2
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr1 = StorageManager::open(dir.path()).unwrap();

        // Write some data
        let artifact = Artifact::new(
            ArtifactKind::Snapshot,
            b"idempotent test".to_vec(),
            "text/plain".into(),
        );
        mgr1.artifact_store().put(&artifact).unwrap();

        // Open again
        let mgr2 = StorageManager::open(dir.path()).unwrap();
        assert!(mgr2.artifact_store().exists(&artifact.id).unwrap());
    }

    #[test]
    fn all_stores_accessible() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = StorageManager::open(dir.path()).unwrap();

        // Artifact store
        let artifact = Artifact::new(
            ArtifactKind::Plan,
            b"test artifact".to_vec(),
            "text/plain".into(),
        );
        mgr.artifact_store().put(&artifact).unwrap();

        // Snapshot store
        let snap = StateSnapshot::initial(VesselId::new(), "test".into());
        mgr.snapshot_store().save(&snap).unwrap();

        // Event ledger
        let entry = exoskeleton_core::EventEntry {
            id: exoskeleton_core::LedgerEntryId::new(),
            tick_id: None,
            event_type: exoskeleton_core::EventType::VesselStarted,
            payload_ref: None,
            summary: "test".into(),
            timestamp: chrono::Utc::now(),
        };
        mgr.event_ledger().append(&entry).unwrap();

        // Tick store
        assert!(mgr.tick_store().latest().unwrap().is_none());
    }

    #[test]
    fn stores_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = StorageManager::open(dir.path()).unwrap();

        // Write to artifact store
        let artifact = Artifact::new(
            ArtifactKind::Receipt,
            b"independence test".to_vec(),
            "text/plain".into(),
        );
        mgr.artifact_store().put(&artifact).unwrap();

        // Snapshot store should be unaffected
        assert!(mgr.snapshot_store().latest().unwrap().is_none());
        assert!(mgr.tick_store().latest().unwrap().is_none());
    }

    // ── T-7: Storage Manager Extension (Sprint 3) ──

    #[test]
    fn open_creates_memory_db() {
        let dir = tempfile::tempdir().unwrap();
        let _mgr = StorageManager::open(dir.path()).unwrap();
        assert!(dir.path().join("exo").join("memory.db").exists());
    }

    #[test]
    fn memory_store_accessible() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = StorageManager::open(dir.path()).unwrap();

        let summary = EpisodicSummary {
            id: ArtifactId::from_content(b"mgr-test-ep"),
            start_tick: 0,
            end_tick: 5,
            summary: "Test via StorageManager".into(),
            key_events: Vec::new(),
            token_count: 10,
            created_at: chrono::Utc::now(),
        };
        mgr.memory_store().write_episodic(&summary).unwrap();
        assert_eq!(mgr.memory_store().count_episodic().unwrap(), 1);
    }

    #[test]
    fn memory_store_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();

        // Write via first manager
        {
            let mgr = StorageManager::open(dir.path()).unwrap();
            let summary = EpisodicSummary {
                id: ArtifactId::from_content(b"reopen-ep"),
                start_tick: 0,
                end_tick: 5,
                summary: "Survives reopen".into(),
                key_events: vec!["key event".into()],
                token_count: 10,
                created_at: chrono::Utc::now(),
            };
            mgr.memory_store().write_episodic(&summary).unwrap();

            let note = LongTermNote {
                id: ArtifactId::from_content(b"reopen-lt"),
                topic: "test".into(),
                content: "Survives reopen".into(),
                tags: Vec::new(),
                token_count: 5,
                created_at: chrono::Utc::now(),
            };
            mgr.memory_store().write_long_term(&note).unwrap();
        }
        // Drop mgr — connection closed

        // Reopen and verify
        let mgr2 = StorageManager::open(dir.path()).unwrap();
        assert_eq!(mgr2.memory_store().count_episodic().unwrap(), 1);
        assert_eq!(mgr2.memory_store().count_long_term().unwrap(), 1);

        let ep = mgr2.memory_store().recent_episodic(1).unwrap();
        assert_eq!(ep[0].summary, "Survives reopen");

        let lt = mgr2.memory_store().all_long_term(1).unwrap();
        assert_eq!(lt[0].content, "Survives reopen");
    }

    #[test]
    fn all_nine_stores_accessible() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = StorageManager::open(dir.path()).unwrap();

        // Artifact store
        let artifact = Artifact::new(
            ArtifactKind::Plan,
            b"seven stores".to_vec(),
            "text/plain".into(),
        );
        mgr.artifact_store().put(&artifact).unwrap();

        // Snapshot store
        let snap = StateSnapshot::initial(VesselId::new(), "test".into());
        mgr.snapshot_store().save(&snap).unwrap();

        // Event ledger
        let entry = exoskeleton_core::EventEntry {
            id: exoskeleton_core::LedgerEntryId::new(),
            tick_id: None,
            event_type: exoskeleton_core::EventType::VesselStarted,
            payload_ref: None,
            summary: "test".into(),
            timestamp: chrono::Utc::now(),
        };
        mgr.event_ledger().append(&entry).unwrap();

        // Tick store
        assert!(mgr.tick_store().latest().unwrap().is_none());

        // Memory store
        let note = LongTermNote {
            id: ArtifactId::from_content(b"seven-stores-lt"),
            topic: "test".into(),
            content: "All seven stores work".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: chrono::Utc::now(),
        };
        mgr.memory_store().write_long_term(&note).unwrap();
        assert_eq!(mgr.memory_store().count_long_term().unwrap(), 1);

        // Thread store
        let thread_spec = exoskeleton_core::ThreadSpec {
            thread_id: exoskeleton_core::ThreadId::new(),
            name: "test thread".into(),
            charter: "Test charter".into(),
            priority: exoskeleton_core::ThreadPriority::Normal,
            token_budget: 4096,
            schedule: exoskeleton_core::ThreadSchedule::EveryTick,
        };
        exoskeleton_threads::ThreadStore::save(
            mgr.thread_store().as_ref(),
            &thread_spec,
            exoskeleton_core::ThreadStatus::Active,
        )
        .unwrap();
        let result = exoskeleton_threads::ThreadStore::get(
            mgr.thread_store().as_ref(),
            thread_spec.thread_id,
        )
        .unwrap();
        assert!(result.is_some());

        // Relationship store (Sprint 8)
        let record = exoskeleton_core::RelationshipRecord {
            id: exoskeleton_core::LedgerEntryId::new(),
            principal_id: exoskeleton_core::PrincipalId::new(),
            signal_type: exoskeleton_core::RelationalSignalType::TrustUpdate,
            content_ref: ArtifactId::from_content(b"seven-stores-rel"),
            tick_id: exoskeleton_core::TickId::new(),
            timestamp: chrono::Utc::now(),
            metadata: Default::default(),
        };
        exoskeleton_relationship::RelationshipLedger::append(
            mgr.relationship_store().as_ref(),
            &record,
        )
        .unwrap();
        assert_eq!(
            exoskeleton_relationship::RelationshipLedger::count(mgr.relationship_store().as_ref())
                .unwrap(),
            1
        );

        // Budget store (Sprint 9)
        assert!(
            exoskeleton_core::BudgetStore::load(mgr.budget_store().as_ref())
                .unwrap()
                .is_none()
        );

        // Conversation store (E1-S2)
        use exoskeleton_core::conversation::ConversationStore;
        let conv = exoskeleton_core::Conversation::from_first_message(
            exoskeleton_core::PrincipalId::new(),
            exoskeleton_core::EnvelopeId::new(),
            ArtifactId::from_content(b"nine-stores-conv"),
            chrono::Utc::now(),
        );
        mgr.conversation_store().save(&conv).unwrap();
        assert!(mgr.conversation_store().get(conv.id).unwrap().is_some());
    }

    // ── E3-S3: StorageManager::create_fresh ──

    #[test]
    fn create_fresh_succeeds_on_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let _mgr = StorageManager::create_fresh(dir.path()).unwrap();
        let exo_dir = dir.path().join("exo");
        assert!(exo_dir.join("artifacts.db").exists());
        assert!(exo_dir.join("snapshots.db").exists());
        assert!(exo_dir.join("events.db").exists());
        assert!(exo_dir.join("ticks.db").exists());
        assert!(exo_dir.join("memory.db").exists());
        assert!(exo_dir.join("threads.db").exists());
        assert!(exo_dir.join("relationships.db").exists());
        assert!(exo_dir.join("budget.db").exists());
        assert!(exo_dir.join("conversations.db").exists());
    }

    #[test]
    fn create_fresh_rejects_existing_stores() {
        let dir = tempfile::tempdir().unwrap();
        let _mgr1 = StorageManager::open(dir.path()).unwrap();
        // Second call to create_fresh should fail
        let result = StorageManager::create_fresh(dir.path());
        assert!(
            matches!(result, Err(ExoError::Storage(msg)) if msg.contains("already has stores"))
        );
    }
}
