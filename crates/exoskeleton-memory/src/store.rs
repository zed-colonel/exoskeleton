//! MemoryStore trait — contract for persisting episodic summaries and long-term notes.
//!
//! The trait lives in `exoskeleton-memory` (not `exoskeleton-core`) because it
//! is specific to the memory subsystem. The SQLite implementation lives in
//! `exoskeleton-host::storage::memory_store`.

use exoskeleton_core::{ArtifactId, EpisodicSummary, ExoError, LongTermNote};

/// Durable store for memory tiers (episodic summaries and long-term notes).
///
/// Both tiers are persisted across restarts. Working memory (tier 0) is
/// ephemeral and not stored here — it exists only as the ContextSources
/// struct during compilation.
///
/// Each entry written to the MemoryStore is also stored as an Artifact
/// (ArtifactKind::Memory) by the caller, ensuring content-addressed
/// deduplication and replay capability (I3).
pub trait MemoryStore: Send + Sync {
    /// Write an episodic summary. Returns the ArtifactId.
    ///
    /// If a summary with the same ArtifactId already exists, this is a
    /// no-op (content-addressed deduplication, same as ArtifactStore).
    fn write_episodic(&self, summary: &EpisodicSummary) -> Result<ArtifactId, ExoError>;

    /// Retrieve the N most recent episodic summaries, newest first.
    ///
    /// Ordered by `end_tick DESC` — most recent span first.
    fn recent_episodic(&self, limit: usize) -> Result<Vec<EpisodicSummary>, ExoError>;

    /// Write a long-term note. Returns the ArtifactId.
    ///
    /// If a note with the same ArtifactId already exists, this is a
    /// no-op (content-addressed deduplication).
    fn write_long_term(&self, note: &LongTermNote) -> Result<ArtifactId, ExoError>;

    /// Search long-term notes by keyword.
    ///
    /// Matches against topic and content using case-insensitive substring
    /// matching. Returns results ordered by `created_at DESC` (newest first).
    ///
    /// For v1.0-alpha, this is a simple LIKE-based search. Future sprints
    /// may add FTS5 or embedding-based retrieval.
    fn search_long_term(&self, query: &str, limit: usize) -> Result<Vec<LongTermNote>, ExoError>;

    /// Retrieve all long-term notes, newest first.
    ///
    /// For small note collections (expected for v1.0-alpha), this is
    /// efficient. A paginated API can be added if the collection grows large.
    fn all_long_term(&self, limit: usize) -> Result<Vec<LongTermNote>, ExoError>;

    /// Count the total number of episodic summaries.
    fn count_episodic(&self) -> Result<u64, ExoError>;

    /// Count the total number of long-term notes.
    fn count_long_term(&self) -> Result<u64, ExoError>;

    /// Evict episodic summaries beyond a given capacity (E1-S3, W-16).
    ///
    /// Deletes the oldest entries (by `end_tick ASC`) until at most `capacity`
    /// entries remain. Returns the count of entries evicted.
    ///
    /// This is called in the Amend step AFTER Memory Consolidation outputs are
    /// processed, ensuring important entries get promoted to long-term notes
    /// before eviction (consolidation-before-eviction guarantee).
    ///
    /// Evicted entries remain as artifacts in the ArtifactStore (I3: audit trail).
    /// Only the MemoryStore row is deleted.
    fn evict_episodic_beyond(&self, capacity: u64) -> Result<u64, ExoError>;

    /// Delete a specific episodic summary by its ArtifactId.
    ///
    /// Returns `true` if the entry existed and was deleted, `false` otherwise.
    fn delete_episodic(&self, id: &ArtifactId) -> Result<bool, ExoError>;
}
