//! Executable thread registry and persistence abstractions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use exoskeleton_core::{
    ExecThreadKind, ExecThreadLocalState, ExecThreadOutput, ExecThreadSpec, ExecThreadStatus,
    ExoError, PromptRegistry, ThreadId,
};

pub trait ExecThreadStore: Send + Sync {
    fn save(&self, spec: &ExecThreadSpec, status: ExecThreadStatus) -> Result<(), ExoError>;
    fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ExecThreadSpec, ExecThreadStatus)>, ExoError>;
    fn list(&self) -> Result<Vec<(ExecThreadSpec, ExecThreadStatus)>, ExoError>;
    fn update_status(&self, thread_id: ThreadId, status: ExecThreadStatus) -> Result<(), ExoError>;
    fn get_local_state(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<ExecThreadLocalState>, ExoError>;
    fn save_local_state(
        &self,
        thread_id: ThreadId,
        local_state: &ExecThreadLocalState,
    ) -> Result<(), ExoError>;
    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ExecThreadOutput>, ExoError>;
    fn save_output(&self, output: &ExecThreadOutput) -> Result<(), ExoError>;
}

pub struct InMemoryExecThreadStore {
    specs: RwLock<HashMap<ThreadId, (ExecThreadSpec, ExecThreadStatus)>>,
    local_state: RwLock<HashMap<ThreadId, ExecThreadLocalState>>,
    outputs: RwLock<HashMap<ThreadId, Vec<ExecThreadOutput>>>,
}

impl InMemoryExecThreadStore {
    pub fn new() -> Self {
        Self {
            specs: RwLock::new(HashMap::new()),
            local_state: RwLock::new(HashMap::new()),
            outputs: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryExecThreadStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecThreadStore for InMemoryExecThreadStore {
    fn save(&self, spec: &ExecThreadSpec, status: ExecThreadStatus) -> Result<(), ExoError> {
        self.specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?
            .insert(spec.thread_id, (spec.clone(), status));
        Ok(())
    }

    fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ExecThreadSpec, ExecThreadStatus)>, ExoError> {
        Ok(self
            .specs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?
            .get(&thread_id)
            .cloned())
    }

    fn list(&self) -> Result<Vec<(ExecThreadSpec, ExecThreadStatus)>, ExoError> {
        Ok(self
            .specs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?
            .values()
            .cloned()
            .collect())
    }

    fn update_status(&self, thread_id: ThreadId, status: ExecThreadStatus) -> Result<(), ExoError> {
        let mut guard = self
            .specs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let entry = guard
            .get_mut(&thread_id)
            .ok_or_else(|| ExoError::Storage("exec thread not found".into()))?;
        entry.1 = status;
        Ok(())
    }

    fn get_local_state(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<ExecThreadLocalState>, ExoError> {
        Ok(self
            .local_state
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?
            .get(&thread_id)
            .cloned())
    }

    fn save_local_state(
        &self,
        thread_id: ThreadId,
        local_state: &ExecThreadLocalState,
    ) -> Result<(), ExoError> {
        self.local_state
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?
            .insert(thread_id, local_state.clone());
        Ok(())
    }

    fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ExecThreadOutput>, ExoError> {
        let guard = self
            .outputs
            .read()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        let Some(outputs) = guard.get(&thread_id) else {
            return Ok(Vec::new());
        };
        let len = outputs.len();
        let start = len.saturating_sub(limit);
        let mut result = outputs[start..].to_vec();
        result.reverse();
        Ok(result)
    }

    fn save_output(&self, output: &ExecThreadOutput) -> Result<(), ExoError> {
        self.outputs
            .write()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?
            .entry(output.thread_id)
            .or_default()
            .push(output.clone());
        Ok(())
    }
}

pub struct ExecThreadRegistry {
    store: Arc<dyn ExecThreadStore>,
}

impl ExecThreadRegistry {
    pub fn new(store: Arc<dyn ExecThreadStore>) -> Self {
        Self { store }
    }

    pub fn register(&self, spec: ExecThreadSpec) -> Result<ThreadId, ExoError> {
        let existing = self.list()?;
        if existing.iter().any(|(existing_spec, status)| {
            existing_spec.kind == spec.kind
                && existing_spec.thread_id != spec.thread_id
                && *status != ExecThreadStatus::Failed
        }) {
            return Err(ExoError::Config(format!(
                "an exec thread for kind {:?} is already registered",
                spec.kind
            )));
        }
        let id = spec.thread_id;
        self.store.save(&spec, ExecThreadStatus::Active)?;
        Ok(id)
    }

    pub fn get(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ExecThreadSpec, ExecThreadStatus)>, ExoError> {
        self.store.get(thread_id)
    }

    pub fn list(&self) -> Result<Vec<(ExecThreadSpec, ExecThreadStatus)>, ExoError> {
        self.store.list()
    }

    pub fn list_active(&self) -> Result<Vec<ExecThreadSpec>, ExoError> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .filter(|(_, status)| *status == ExecThreadStatus::Active)
            .map(|(spec, _)| spec)
            .collect())
    }

    pub fn save_output(&self, output: &ExecThreadOutput) -> Result<(), ExoError> {
        self.store.save_output(output)
    }

    pub fn update_status(
        &self,
        thread_id: ThreadId,
        status: ExecThreadStatus,
    ) -> Result<(), ExoError> {
        self.store.update_status(thread_id, status)
    }

    pub fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ExecThreadOutput>, ExoError> {
        self.store.recent_outputs(thread_id, limit)
    }

    pub fn local_state(&self, thread_id: ThreadId) -> Result<ExecThreadLocalState, ExoError> {
        Ok(self.store.get_local_state(thread_id)?.unwrap_or_default())
    }

    pub fn save_local_state(
        &self,
        thread_id: ThreadId,
        state: &ExecThreadLocalState,
    ) -> Result<(), ExoError> {
        self.store.save_local_state(thread_id, state)
    }

    pub fn exec_thread_summaries(
        &self,
    ) -> Result<Vec<exoskeleton_core::ExecThreadSummary>, ExoError> {
        let all = self.store.list()?;
        let mut out = Vec::with_capacity(all.len());
        for (spec, status) in all {
            let local_state = self
                .store
                .get_local_state(spec.thread_id)?
                .unwrap_or_default();
            let last_output_summary = self
                .store
                .recent_outputs(spec.thread_id, 1)?
                .first()
                .map(|o| o.summary.clone());
            out.push(exoskeleton_core::ExecThreadSummary {
                thread_id: spec.thread_id,
                kind: spec.kind,
                name: spec.name,
                status,
                last_output_summary,
                current_focus: local_state.current_focus,
                work_phase: local_state.work_phase,
                evidence_complete: local_state.evidence_complete,
                proposal_confidence: local_state.proposal_confidence,
                last_completion_reason: local_state.last_completion_reason,
            });
        }
        Ok(out)
    }
}

pub const CODING_EXEC_THREAD_ID: ThreadId = ThreadId::from_uuid(uuid::Uuid::from_bytes([
    0xca, 0xe1, 0x10, 0x01, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]));

pub fn register_builtin_exec_threads(
    registry: &ExecThreadRegistry,
    prompts: &PromptRegistry,
    workspace_root: Option<String>,
    enabled: bool,
) -> Result<(), ExoError> {
    if !enabled {
        return Ok(());
    }

    if registry.get(CODING_EXEC_THREAD_ID)?.is_none() {
        let charter = prompts
            .get("charter-coding-thread")
            .unwrap_or("Track coding progress, maintain local work state, and propose the next external action to advance the task.")
            .to_string();
        let thread_id = registry.register(ExecThreadSpec {
            thread_id: CODING_EXEC_THREAD_ID,
            kind: ExecThreadKind::Coding,
            name: "Coding".into(),
            charter,
            token_budget: 8_000,
            workspace_root,
        })?;
        registry.update_status(thread_id, ExecThreadStatus::Idle)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: ExecThreadKind) -> ExecThreadSpec {
        ExecThreadSpec {
            thread_id: ThreadId::new(),
            kind,
            name: "test".into(),
            charter: "charter".into(),
            token_budget: 1000,
            workspace_root: None,
        }
    }

    #[test]
    fn register_rejects_duplicate_kind_when_existing_thread_is_idle() {
        let registry = ExecThreadRegistry::new(Arc::new(InMemoryExecThreadStore::new()));
        let first = spec(ExecThreadKind::Coding);
        let second = spec(ExecThreadKind::Coding);

        registry.register(first.clone()).unwrap();
        registry
            .update_status(first.thread_id, ExecThreadStatus::Idle)
            .unwrap();

        let err = registry.register(second).unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn register_allows_replacement_after_failed_thread() {
        let registry = ExecThreadRegistry::new(Arc::new(InMemoryExecThreadStore::new()));
        let first = spec(ExecThreadKind::Coding);
        let second = spec(ExecThreadKind::Coding);

        registry.register(first.clone()).unwrap();
        registry
            .update_status(first.thread_id, ExecThreadStatus::Failed)
            .unwrap();

        registry.register(second).unwrap();
    }
}
