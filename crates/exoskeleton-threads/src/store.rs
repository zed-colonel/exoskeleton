//! Thread persistence layer.
//!
//! The `ThreadStore` trait provides durable storage for thread specifications
//! and operational state. Implementations must be `Send + Sync`.

use std::collections::HashMap;
use std::sync::RwLock;

use exoskeleton_core::{ExoError, ThreadId, ThreadOutput, ThreadSpec, ThreadStatus};

/// Durable storage for thread specifications and operational state.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks and threads within the Cognitive AQ.
pub trait ThreadStore: Send + Sync {
    /// Save (upsert) a thread specification with its current status.
    fn save(&self, spec: &ThreadSpec, status: ThreadStatus) -> Result<(), ExoError>;

    /// Retrieve a thread specification and its status by ID.
    fn get(&self, thread_id: ThreadId) -> Result<Option<(ThreadSpec, ThreadStatus)>, ExoError>;

    /// List all stored thread specifications with their statuses.
    fn list(&self) -> Result<Vec<(ThreadSpec, ThreadStatus)>, ExoError>;

    /// Remove a thread by ID. Idempotent: returns `Ok(())` even if not found.
    fn remove(&self, thread_id: ThreadId) -> Result<(), ExoError>;

    /// Update the status of an existing thread.
    ///
    /// Returns `ExoError::Storage` if the thread ID is not found.
    fn update_status(&self, thread_id: ThreadId, status: ThreadStatus) -> Result<(), ExoError>;

    /// Record the tick number of the last execution for a thread.
    fn save_last_run(&self, thread_id: ThreadId, tick_number: u64) -> Result<(), ExoError>;

    /// Retrieve the tick number of the last execution for a thread.
    fn get_last_run(&self, thread_id: ThreadId) -> Result<Option<u64>, ExoError>;

    /// Retrieve the most recent outputs for a thread, newest first.
    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadOutput>, ExoError>;

    /// Append an output to the thread's output history.
    fn save_output(&self, output: &ThreadOutput) -> Result<(), ExoError>;
}

/// In-memory thread store for testing.
///
/// All data is held in `RwLock`-guarded `HashMap`s. Not suitable for
/// production use since data is lost on process exit.
pub struct InMemoryThreadStore {
    specs: RwLock<HashMap<ThreadId, (ThreadSpec, ThreadStatus)>>,
    last_runs: RwLock<HashMap<ThreadId, u64>>,
    outputs: RwLock<HashMap<ThreadId, Vec<ThreadOutput>>>,
}

impl InMemoryThreadStore {
    /// Create a new, empty in-memory thread store.
    pub fn new() -> Self {
        Self {
            specs: RwLock::new(HashMap::new()),
            last_runs: RwLock::new(HashMap::new()),
            outputs: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryThreadStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadStore for InMemoryThreadStore {
    fn save(&self, spec: &ThreadSpec, status: ThreadStatus) -> Result<(), ExoError> {
        let mut specs = self
            .specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        specs.insert(spec.thread_id, (spec.clone(), status));
        Ok(())
    }

    fn get(&self, thread_id: ThreadId) -> Result<Option<(ThreadSpec, ThreadStatus)>, ExoError> {
        let specs = self
            .specs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(specs.get(&thread_id).cloned())
    }

    fn list(&self) -> Result<Vec<(ThreadSpec, ThreadStatus)>, ExoError> {
        let specs = self
            .specs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(specs.values().cloned().collect())
    }

    fn remove(&self, thread_id: ThreadId) -> Result<(), ExoError> {
        let mut specs = self
            .specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        specs.remove(&thread_id);
        Ok(())
    }

    fn update_status(&self, thread_id: ThreadId, status: ThreadStatus) -> Result<(), ExoError> {
        let mut specs = self
            .specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        match specs.get_mut(&thread_id) {
            Some(entry) => {
                entry.1 = status;
                Ok(())
            }
            None => Err(ExoError::Storage("thread not found".into())),
        }
    }

    fn save_last_run(&self, thread_id: ThreadId, tick_number: u64) -> Result<(), ExoError> {
        let mut last_runs = self
            .last_runs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        last_runs.insert(thread_id, tick_number);
        Ok(())
    }

    fn get_last_run(&self, thread_id: ThreadId) -> Result<Option<u64>, ExoError> {
        let last_runs = self
            .last_runs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(last_runs.get(&thread_id).copied())
    }

    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadOutput>, ExoError> {
        let outputs = self
            .outputs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        match outputs.get(&thread_id) {
            Some(thread_outputs) => {
                // Return newest first, up to `limit`.
                let len = thread_outputs.len();
                let start = len.saturating_sub(limit);
                let mut result: Vec<ThreadOutput> = thread_outputs[start..].to_vec();
                result.reverse();
                Ok(result)
            }
            None => Ok(Vec::new()),
        }
    }

    fn save_output(&self, output: &ThreadOutput) -> Result<(), ExoError> {
        let mut outputs = self
            .outputs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        outputs
            .entry(output.thread_id)
            .or_default()
            .push(output.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        ArtifactId, ThreadId, ThreadPriority, ThreadSchedule, ThreadSpec, ThreadStatus, TickId,
    };

    use super::*;

    /// Helper: create a `ThreadSpec` with sensible defaults for testing.
    fn make_spec(name: &str) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            name: name.into(),
            charter: format!("Charter for {name}"),
            priority: ThreadPriority::Normal,
            token_budget: 4096,
            schedule: ThreadSchedule::EveryTick,
        }
    }

    /// Helper: create a `ThreadOutput` for a given thread.
    fn make_output(thread_id: ThreadId, summary: &str) -> ThreadOutput {
        ThreadOutput {
            thread_id,
            tick_id: TickId::new(),
            artifact_id: ArtifactId::from_content(summary.as_bytes()),
            summary: summary.into(),
            recommendations: vec![format!("rec from {summary}")],
        }
    }

    #[test]
    fn store_save_and_get() {
        let store = InMemoryThreadStore::new();
        let spec = make_spec("Alpha");

        store.save(&spec, ThreadStatus::Active).unwrap();

        let result = store.get(spec.thread_id).unwrap();
        assert!(result.is_some());
        let (got_spec, got_status) = result.unwrap();
        assert_eq!(got_spec, spec);
        assert_eq!(got_status, ThreadStatus::Active);
    }

    #[test]
    fn store_list_returns_all() {
        let store = InMemoryThreadStore::new();
        let specs: Vec<ThreadSpec> = vec![make_spec("One"), make_spec("Two"), make_spec("Three")];

        for spec in &specs {
            store.save(spec, ThreadStatus::Active).unwrap();
        }

        let list = store.list().unwrap();
        assert_eq!(list.len(), 3);

        // All three thread IDs should be present.
        for spec in &specs {
            assert!(
                list.iter().any(|(s, _)| s.thread_id == spec.thread_id),
                "Missing thread {}",
                spec.name
            );
        }
    }

    #[test]
    fn store_remove_idempotent() {
        let store = InMemoryThreadStore::new();
        let spec = make_spec("Removable");

        store.save(&spec, ThreadStatus::Active).unwrap();
        store.remove(spec.thread_id).unwrap();
        assert!(store.get(spec.thread_id).unwrap().is_none());

        // Removing again should succeed (idempotent).
        store.remove(spec.thread_id).unwrap();

        // Removing a never-inserted ID should also succeed.
        store.remove(ThreadId::new()).unwrap();
    }

    #[test]
    fn store_update_status() {
        let store = InMemoryThreadStore::new();
        let spec = make_spec("Updatable");

        store.save(&spec, ThreadStatus::Active).unwrap();
        store
            .update_status(spec.thread_id, ThreadStatus::Suspended)
            .unwrap();

        let (_, status) = store.get(spec.thread_id).unwrap().unwrap();
        assert_eq!(status, ThreadStatus::Suspended);
    }

    #[test]
    fn store_update_status_nonexistent_fails() {
        let store = InMemoryThreadStore::new();
        let result = store.update_status(ThreadId::new(), ThreadStatus::Active);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("thread not found"),
            "Expected 'thread not found' in error, got: {msg}"
        );
    }

    #[test]
    fn store_save_last_run_and_get() {
        let store = InMemoryThreadStore::new();
        let tid = ThreadId::new();

        // Initially no last run.
        assert!(store.get_last_run(tid).unwrap().is_none());

        store.save_last_run(tid, 5).unwrap();
        assert_eq!(store.get_last_run(tid).unwrap(), Some(5));

        // Overwrite with a later tick.
        store.save_last_run(tid, 12).unwrap();
        assert_eq!(store.get_last_run(tid).unwrap(), Some(12));
    }

    #[test]
    fn store_recent_outputs_ordered() {
        let store = InMemoryThreadStore::new();
        let tid = ThreadId::new();

        // Save 5 outputs in chronological order.
        for i in 0..5 {
            let output = ThreadOutput {
                thread_id: tid,
                tick_id: TickId::new(),
                artifact_id: ArtifactId::from_content(format!("output-{i}").as_bytes()),
                summary: format!("output-{i}"),
                recommendations: vec![],
            };
            store.save_output(&output).unwrap();
        }

        // Request 3 most recent -- should be newest first.
        let recent = store.recent_outputs(tid, 3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].summary, "output-4");
        assert_eq!(recent[1].summary, "output-3");
        assert_eq!(recent[2].summary, "output-2");
    }

    #[test]
    fn store_save_output_and_retrieve() {
        let store = InMemoryThreadStore::new();
        let tid = ThreadId::new();
        let output = make_output(tid, "test-output");

        store.save_output(&output).unwrap();

        let recent = store.recent_outputs(tid, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].thread_id, tid);
        assert_eq!(recent[0].summary, "test-output");
        assert_eq!(recent[0].recommendations, vec!["rec from test-output"]);
        assert_eq!(recent[0].artifact_id, output.artifact_id);
    }
}
