//! Thread persistence layer.
//!
//! The `ThreadStore` trait provides durable storage for both cognitive and
//! executable thread specifications plus their operational state.

use std::collections::HashMap;
use std::sync::RwLock;

use exoskeleton_core::{
    ExecThreadLocalState, ExecThreadOutput, ExecThreadStatus, ExoError, ThreadExecutionPayload,
    ThreadExecutionResult, ThreadId, ThreadOutput, ThreadSpec, ThreadStatus,
};

/// Flavor-aware operational status stored for a registered thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisteredThreadStatus {
    Cognitive(ThreadStatus),
    Executable(ExecThreadStatus),
}

impl RegisteredThreadStatus {
    pub fn cognitive_active() -> Self {
        Self::Cognitive(ThreadStatus::Active)
    }

    pub fn executable_active() -> Self {
        Self::Executable(ExecThreadStatus::Active)
    }

    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Cognitive(status) => status.is_terminal(),
            Self::Executable(status) => status.is_terminal(),
        }
    }
}

/// Durable storage for thread specifications and operational state.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks and threads within the vessel runtime.
pub trait ThreadStore: Send + Sync {
    /// Save (upsert) a thread specification with its current status.
    fn save(&self, spec: &ThreadSpec, status: RegisteredThreadStatus) -> Result<(), ExoError>;

    /// Retrieve a thread specification and its flavor-aware status by ID.
    fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ThreadSpec, RegisteredThreadStatus)>, ExoError>;

    /// List all stored thread specifications with their statuses.
    fn list(&self) -> Result<Vec<(ThreadSpec, RegisteredThreadStatus)>, ExoError>;

    /// Remove a thread by ID. Idempotent: returns `Ok(())` even if not found.
    fn remove(&self, thread_id: ThreadId) -> Result<(), ExoError>;

    /// Update the status of an existing thread.
    fn update_status(
        &self,
        thread_id: ThreadId,
        status: RegisteredThreadStatus,
    ) -> Result<(), ExoError>;

    /// Record the tick number of the last execution for a thread.
    fn save_last_run(&self, thread_id: ThreadId, tick_number: u64) -> Result<(), ExoError>;

    /// Retrieve the tick number of the last execution for a thread.
    fn get_last_run(&self, thread_id: ThreadId) -> Result<Option<u64>, ExoError>;

    /// Retrieve the most recent execution results for a thread, newest first.
    fn recent_execution_results(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadExecutionResult>, ExoError>;

    /// Append an execution result to the thread's output history.
    fn save_execution_result(&self, output: &ThreadExecutionResult) -> Result<(), ExoError>;

    /// Retrieve executable-thread local state if present.
    fn get_local_state(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<ExecThreadLocalState>, ExoError>;

    /// Persist executable-thread local state.
    fn save_local_state(
        &self,
        thread_id: ThreadId,
        local_state: &ExecThreadLocalState,
    ) -> Result<(), ExoError>;

    /// Update the charter text of an existing thread.
    fn update_charter(&self, thread_id: ThreadId, charter: String) -> Result<(), ExoError>;

    /// Retrieve the most recent cognitive outputs for a thread, newest first.
    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadOutput>, ExoError> {
        Ok(self
            .recent_execution_results(thread_id, limit)?
            .into_iter()
            .filter_map(|result| match result.payload {
                ThreadExecutionPayload::Cognitive(output) => Some(output),
                ThreadExecutionPayload::Executable(_) => None,
            })
            .collect())
    }

    /// Append a cognitive-thread output to the thread's output history.
    fn save_output(&self, output: &ThreadOutput) -> Result<(), ExoError> {
        self.save_execution_result(&output.clone().into())
    }

    /// Retrieve the most recent executable outputs for a thread, newest first.
    fn recent_exec_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ExecThreadOutput>, ExoError> {
        Ok(self
            .recent_execution_results(thread_id, limit)?
            .into_iter()
            .filter_map(|result| match result.payload {
                ThreadExecutionPayload::Cognitive(_) => None,
                ThreadExecutionPayload::Executable(output) => Some(output),
            })
            .collect())
    }

    /// Append an executable-thread output to the thread's output history.
    fn save_exec_output(&self, output: &ExecThreadOutput) -> Result<(), ExoError> {
        self.save_execution_result(&output.clone().into())
    }
}

/// In-memory thread store for testing.
pub struct InMemoryThreadStore {
    specs: RwLock<HashMap<ThreadId, (ThreadSpec, RegisteredThreadStatus)>>,
    last_runs: RwLock<HashMap<ThreadId, u64>>,
    outputs: RwLock<HashMap<ThreadId, Vec<ThreadExecutionResult>>>,
    local_state: RwLock<HashMap<ThreadId, ExecThreadLocalState>>,
}

impl InMemoryThreadStore {
    pub fn new() -> Self {
        Self {
            specs: RwLock::new(HashMap::new()),
            last_runs: RwLock::new(HashMap::new()),
            outputs: RwLock::new(HashMap::new()),
            local_state: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryThreadStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadStore for InMemoryThreadStore {
    fn save(&self, spec: &ThreadSpec, status: RegisteredThreadStatus) -> Result<(), ExoError> {
        let mut specs = self
            .specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        specs.insert(spec.thread_id, (spec.clone(), status));
        Ok(())
    }

    fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
        let specs = self
            .specs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(specs.get(&thread_id).cloned())
    }

    fn list(&self) -> Result<Vec<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
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

    fn update_status(
        &self,
        thread_id: ThreadId,
        status: RegisteredThreadStatus,
    ) -> Result<(), ExoError> {
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

    fn recent_execution_results(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadExecutionResult>, ExoError> {
        let outputs = self
            .outputs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        match outputs.get(&thread_id) {
            Some(thread_outputs) => {
                let len = thread_outputs.len();
                let start = len.saturating_sub(limit);
                let mut result: Vec<ThreadExecutionResult> = thread_outputs[start..].to_vec();
                result.reverse();
                Ok(result)
            }
            None => Ok(Vec::new()),
        }
    }

    fn save_execution_result(&self, output: &ThreadExecutionResult) -> Result<(), ExoError> {
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

    fn get_local_state(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<ExecThreadLocalState>, ExoError> {
        let local_state = self
            .local_state
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(local_state.get(&thread_id).cloned())
    }

    fn save_local_state(
        &self,
        thread_id: ThreadId,
        local_state: &ExecThreadLocalState,
    ) -> Result<(), ExoError> {
        let mut guard = self
            .local_state
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        guard.insert(thread_id, local_state.clone());
        Ok(())
    }

    fn update_charter(&self, thread_id: ThreadId, charter: String) -> Result<(), ExoError> {
        let mut specs = self
            .specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        match specs.get_mut(&thread_id) {
            Some(entry) => {
                entry.0.charter = charter;
                Ok(())
            }
            None => Err(ExoError::Storage("thread not found".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        ArtifactId, ExecThreadKind, ExecThreadLocalState, ExecThreadOutput, ExecThreadStatus,
        ThreadExecutionPayload, ThreadFlavor, ThreadId, ThreadPriority, ThreadRole, ThreadSchedule,
        ThreadSpec, ThreadStatus, TickId,
    };

    use super::*;

    fn make_spec(name: &str) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            role: ThreadRole::Other,
            flavor: ThreadFlavor::Cognitive,
            name: name.into(),
            charter: format!("Charter for {name}"),
            priority: ThreadPriority::Normal,
            token_budget: 4096,
            schedule: ThreadSchedule::EveryTick,
            workspace_root: None,
        }
    }

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

        store
            .save(&spec, RegisteredThreadStatus::cognitive_active())
            .unwrap();

        let result = store.get(spec.thread_id).unwrap();
        assert!(result.is_some());
        let (got_spec, got_status) = result.unwrap();
        assert_eq!(got_spec, spec);
        assert_eq!(got_status, RegisteredThreadStatus::cognitive_active());
    }

    #[test]
    fn store_list_returns_all() {
        let store = InMemoryThreadStore::new();
        let specs: Vec<ThreadSpec> = vec![make_spec("One"), make_spec("Two"), make_spec("Three")];

        for spec in &specs {
            store
                .save(spec, RegisteredThreadStatus::cognitive_active())
                .unwrap();
        }

        let list = store.list().unwrap();
        assert_eq!(list.len(), 3);
        for spec in &specs {
            assert!(list.iter().any(|(s, _)| s.thread_id == spec.thread_id));
        }
    }

    #[test]
    fn store_remove_idempotent() {
        let store = InMemoryThreadStore::new();
        let spec = make_spec("Removable");

        store
            .save(&spec, RegisteredThreadStatus::cognitive_active())
            .unwrap();
        store.remove(spec.thread_id).unwrap();
        assert!(store.get(spec.thread_id).unwrap().is_none());
        store.remove(spec.thread_id).unwrap();
        store.remove(ThreadId::new()).unwrap();
    }

    #[test]
    fn store_update_status() {
        let store = InMemoryThreadStore::new();
        let spec = make_spec("Updatable");

        store
            .save(&spec, RegisteredThreadStatus::cognitive_active())
            .unwrap();
        store
            .update_status(
                spec.thread_id,
                RegisteredThreadStatus::Cognitive(ThreadStatus::Suspended),
            )
            .unwrap();

        let (_, status) = store.get(spec.thread_id).unwrap().unwrap();
        assert_eq!(
            status,
            RegisteredThreadStatus::Cognitive(ThreadStatus::Suspended)
        );
    }

    #[test]
    fn store_update_status_nonexistent_fails() {
        let store = InMemoryThreadStore::new();
        let result =
            store.update_status(ThreadId::new(), RegisteredThreadStatus::cognitive_active());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("thread not found"));
    }

    #[test]
    fn store_save_last_run_and_get() {
        let store = InMemoryThreadStore::new();
        let tid = ThreadId::new();

        assert!(store.get_last_run(tid).unwrap().is_none());
        store.save_last_run(tid, 5).unwrap();
        assert_eq!(store.get_last_run(tid).unwrap(), Some(5));
        store.save_last_run(tid, 12).unwrap();
        assert_eq!(store.get_last_run(tid).unwrap(), Some(12));
    }

    #[test]
    fn store_recent_outputs_ordered() {
        let store = InMemoryThreadStore::new();
        let tid = ThreadId::new();

        for i in 0..5 {
            store
                .save_output(&ThreadOutput {
                    thread_id: tid,
                    tick_id: TickId::new(),
                    artifact_id: ArtifactId::from_content(format!("output-{i}").as_bytes()),
                    summary: format!("output-{i}"),
                    recommendations: vec![],
                })
                .unwrap();
        }

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
        assert_eq!(recent, vec![output]);
    }

    #[test]
    fn update_charter_changes_text() {
        let store = InMemoryThreadStore::new();
        let spec = make_spec("Updatable");
        let tid = spec.thread_id;

        store
            .save(&spec, RegisteredThreadStatus::cognitive_active())
            .unwrap();
        store
            .update_charter(tid, "New charter text".into())
            .unwrap();

        let (got_spec, _) = store.get(tid).unwrap().unwrap();
        assert_eq!(got_spec.charter, "New charter text");
    }

    #[test]
    fn update_charter_nonexistent_fails() {
        let store = InMemoryThreadStore::new();
        let result = store.update_charter(ThreadId::new(), "anything".into());
        assert!(result.is_err());
    }

    #[test]
    fn store_roundtrips_exec_local_state() {
        let store = InMemoryThreadStore::new();
        let spec = ThreadSpec {
            flavor: ThreadFlavor::Executable,
            role: ThreadRole::Coding,
            ..make_spec("Coding")
        };
        let state = ExecThreadLocalState {
            current_focus: Some("focus".into()),
            ..ExecThreadLocalState::default()
        };

        store
            .save(&spec, RegisteredThreadStatus::executable_active())
            .unwrap();
        store.save_local_state(spec.thread_id, &state).unwrap();

        assert_eq!(store.get_local_state(spec.thread_id).unwrap(), Some(state));
    }

    #[test]
    fn store_filters_exec_results_out_of_cognitive_recent_outputs() {
        let store = InMemoryThreadStore::new();
        let spec = ThreadSpec {
            flavor: ThreadFlavor::Executable,
            role: ThreadRole::Coding,
            ..make_spec("Coding")
        };
        let exec_output = ExecThreadOutput {
            thread_id: spec.thread_id,
            tick_id: TickId::new(),
            artifact_id: ArtifactId::from_content(b"exec"),
            kind: ExecThreadKind::Coding,
            summary: "exec".into(),
            status: ExecThreadStatus::Active,
            evidence_complete: false,
            proposal_confidence: None,
            proposed_action: None,
            local_state: ExecThreadLocalState::default(),
        };

        store
            .save(&spec, RegisteredThreadStatus::executable_active())
            .unwrap();
        store.save_exec_output(&exec_output).unwrap();

        assert!(store.recent_outputs(spec.thread_id, 5).unwrap().is_empty());
        assert_eq!(
            store.recent_exec_outputs(spec.thread_id, 5).unwrap(),
            vec![exec_output]
        );
        let unified = store.recent_execution_results(spec.thread_id, 5).unwrap();
        assert!(matches!(
            unified[0].payload,
            ThreadExecutionPayload::Executable(_)
        ));
    }
}
