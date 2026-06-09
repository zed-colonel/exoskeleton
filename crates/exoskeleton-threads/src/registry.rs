//! Unified thread lifecycle manager.

use std::sync::Arc;

use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_core::{
    ExecThreadLocalState, ExecThreadOutput, ExecThreadStatus, ExecThreadSummary, ExoError,
    ThreadExecutionResult, ThreadFlavor, ThreadId, ThreadOutput, ThreadRole, ThreadSpec,
    ThreadStatus, ThreadSummary,
};

use crate::scheduling::is_thread_due;
use crate::store::{RegisteredThreadStatus, ThreadStore};

pub struct ThreadRegistry {
    store: Arc<dyn ThreadStore>,
}

impl ThreadRegistry {
    pub fn new(store: Arc<dyn ThreadStore>) -> Self {
        Self { store }
    }

    pub fn register(&self, spec: ThreadSpec) -> Result<ThreadId, ExoError> {
        let default_status = match spec.flavor {
            ThreadFlavor::Cognitive => RegisteredThreadStatus::Cognitive(ThreadStatus::Active),
            ThreadFlavor::Executable => {
                RegisteredThreadStatus::Executable(ExecThreadStatus::Active)
            }
        };
        self.register_with_status(spec, default_status)
    }

    pub fn register_with_status(
        &self,
        spec: ThreadSpec,
        status: RegisteredThreadStatus,
    ) -> Result<ThreadId, ExoError> {
        let id = spec.thread_id;
        self.store.save(&spec, status)?;
        Ok(id)
    }

    pub fn deregister(&self, thread_id: ThreadId) -> Result<(), ExoError> {
        self.store.remove(thread_id)
    }

    /// Cognitive-only getter kept for the existing cognitive thread path.
    pub fn get(&self, thread_id: ThreadId) -> Result<Option<(ThreadSpec, ThreadStatus)>, ExoError> {
        Ok(self
            .get_registered(thread_id)?
            .and_then(|(spec, status)| match status {
                RegisteredThreadStatus::Cognitive(status) => Some((spec, status)),
                RegisteredThreadStatus::Executable(_) => None,
            }))
    }

    pub fn get_registered(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
        self.store.get(thread_id)
    }

    /// Cognitive-only list kept for the existing cognitive thread path.
    pub fn list(&self) -> Result<Vec<(ThreadSpec, ThreadStatus)>, ExoError> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .filter_map(|(spec, status)| match status {
                RegisteredThreadStatus::Cognitive(status) => Some((spec, status)),
                RegisteredThreadStatus::Executable(_) => None,
            })
            .collect())
    }

    pub fn list_registered(&self) -> Result<Vec<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
        self.store.list()
    }

    pub fn list_executable(&self) -> Result<Vec<(ThreadSpec, ExecThreadStatus)>, ExoError> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .filter_map(|(spec, status)| match status {
                RegisteredThreadStatus::Cognitive(_) => None,
                RegisteredThreadStatus::Executable(status) => Some((spec, status)),
            })
            .collect())
    }

    pub fn list_active(&self) -> Result<Vec<ThreadSpec>, ExoError> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .filter_map(|(spec, status)| match status {
                RegisteredThreadStatus::Cognitive(ThreadStatus::Active) => Some(spec),
                _ => None,
            })
            .collect())
    }

    pub fn list_active_executable(&self) -> Result<Vec<ThreadSpec>, ExoError> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .filter_map(|(spec, status)| match status {
                RegisteredThreadStatus::Executable(ExecThreadStatus::Active) => Some(spec),
                _ => None,
            })
            .collect())
    }

    pub fn update_status(&self, thread_id: ThreadId, status: ThreadStatus) -> Result<(), ExoError> {
        let entry = self.store.get(thread_id)?;
        match entry {
            Some((_spec, current_status)) => {
                if current_status.is_terminal() {
                    return Err(ExoError::Engine(format!(
                        "cannot transition from terminal state {:?}",
                        current_status
                    )));
                }
                match current_status {
                    RegisteredThreadStatus::Cognitive(_) => self
                        .store
                        .update_status(thread_id, RegisteredThreadStatus::Cognitive(status)),
                    RegisteredThreadStatus::Executable(_) => Err(ExoError::Config(
                        "cannot apply cognitive thread status to executable thread".into(),
                    )),
                }
            }
            None => Err(ExoError::Storage("thread not found".into())),
        }
    }

    pub fn update_executable_status(
        &self,
        thread_id: ThreadId,
        status: ExecThreadStatus,
    ) -> Result<(), ExoError> {
        let entry = self.store.get(thread_id)?;
        match entry {
            Some((_spec, current_status)) => {
                if current_status.is_terminal() {
                    return Err(ExoError::Engine(format!(
                        "cannot transition from terminal state {:?}",
                        current_status
                    )));
                }
                match current_status {
                    RegisteredThreadStatus::Cognitive(_) => Err(ExoError::Config(
                        "cannot apply executable thread status to cognitive thread".into(),
                    )),
                    RegisteredThreadStatus::Executable(_) => self
                        .store
                        .update_status(thread_id, RegisteredThreadStatus::Executable(status)),
                }
            }
            None => Err(ExoError::Storage("thread not found".into())),
        }
    }

    pub fn record_run(&self, thread_id: ThreadId, tick_number: u64) -> Result<(), ExoError> {
        self.store.save_last_run(thread_id, tick_number)
    }

    pub fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadOutput>, ExoError> {
        self.store.recent_outputs(thread_id, limit)
    }

    pub fn save_output(&self, output: &ThreadOutput) -> Result<(), ExoError> {
        self.store.save_output(output)
    }

    pub fn recent_execution_results(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadExecutionResult>, ExoError> {
        self.store.recent_execution_results(thread_id, limit)
    }

    pub fn save_execution_result(&self, result: &ThreadExecutionResult) -> Result<(), ExoError> {
        self.store.save_execution_result(result)
    }

    pub fn recent_exec_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ExecThreadOutput>, ExoError> {
        self.store.recent_exec_outputs(thread_id, limit)
    }

    pub fn save_exec_output(&self, output: &ExecThreadOutput) -> Result<(), ExoError> {
        self.store.save_exec_output(output)
    }

    pub fn local_state(&self, thread_id: ThreadId) -> Result<ExecThreadLocalState, ExoError> {
        Ok(self.store.get_local_state(thread_id)?.unwrap_or_default())
    }

    pub fn save_local_state(
        &self,
        thread_id: ThreadId,
        local_state: &ExecThreadLocalState,
    ) -> Result<(), ExoError> {
        self.store.save_local_state(thread_id, local_state)
    }

    pub fn update_charter(&self, thread_id: ThreadId, charter: String) -> Result<(), ExoError> {
        self.store.update_charter(thread_id, charter)
    }

    pub fn reload_charters(&self, prompts: &PromptRegistry) -> Result<u32, ExoError> {
        let threads = self.store.list()?;
        let mut updated = 0;
        for (spec, _status) in &threads {
            let key = charter_key(&spec.name);
            if let Some(new_charter) = prompts.get(&key) {
                if new_charter != spec.charter {
                    self.store
                        .update_charter(spec.thread_id, new_charter.to_string())?;
                    updated += 1;
                }
            }
        }
        Ok(updated)
    }

    pub fn due_threads(&self, tick_number: u64) -> Result<Vec<ThreadSpec>, ExoError> {
        let all = self.store.list()?;
        let mut due: Vec<ThreadSpec> = all
            .into_iter()
            .filter_map(|(spec, status)| match status {
                RegisteredThreadStatus::Cognitive(status) => {
                    let last_run = self.store.get_last_run(spec.thread_id).unwrap_or(None);
                    is_thread_due(spec.schedule, status, tick_number, last_run).then_some(spec)
                }
                RegisteredThreadStatus::Executable(_) => None,
            })
            .collect();
        due.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(due)
    }

    pub fn thread_summaries(&self) -> Result<Vec<ThreadSummary>, ExoError> {
        let all = self.store.list()?;
        let mut summaries = Vec::new();
        for (spec, status) in all {
            let RegisteredThreadStatus::Cognitive(status) = status else {
                continue;
            };
            let last_output_summary = self
                .store
                .recent_outputs(spec.thread_id, 1)?
                .first()
                .map(|o| o.summary.clone());
            summaries.push(ThreadSummary {
                thread_id: spec.thread_id,
                name: spec.name,
                status,
                last_output_summary,
                token_budget_remaining: spec.token_budget,
            });
        }
        Ok(summaries)
    }

    pub fn exec_thread_summaries(&self) -> Result<Vec<ExecThreadSummary>, ExoError> {
        let all = self.store.list()?;
        let mut out = Vec::new();
        for (spec, status) in all {
            let RegisteredThreadStatus::Executable(status) = status else {
                continue;
            };
            let local_state = self
                .store
                .get_local_state(spec.thread_id)?
                .unwrap_or_default();
            let last_output_summary = self
                .store
                .recent_exec_outputs(spec.thread_id, 1)?
                .first()
                .map(|o| o.summary.clone());
            let kind =
                exoskeleton_core::ExecThreadKind::try_from(spec.role.clone()).map_err(|_| {
                    ExoError::Config(format!("thread role {:?} is not executable", spec.role))
                })?;
            out.push(ExecThreadSummary {
                thread_id: spec.thread_id,
                kind,
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

    pub fn find_registered_by_role(
        &self,
        role: ThreadRole,
    ) -> Result<Option<(ThreadSpec, RegisteredThreadStatus)>, ExoError> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .find(|(spec, _)| spec.role == role))
    }
}

fn charter_key(name: &str) -> String {
    format!("charter-{}", name.to_lowercase().replace(' ', "-"))
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        ExecThreadStatus, ThreadFlavor, ThreadId, ThreadPriority, ThreadRole, ThreadSchedule,
        ThreadSpec, ThreadStatus,
    };

    use super::*;
    use crate::store::InMemoryThreadStore;

    fn test_spec(name: &str, priority: ThreadPriority, schedule: ThreadSchedule) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            role: ThreadRole::Other,
            flavor: ThreadFlavor::Cognitive,
            name: name.into(),
            charter: format!("Test charter for {name}"),
            priority,
            token_budget: 2000,
            schedule,
            workspace_root: None,
        }
    }

    #[test]
    fn registry_register_and_get() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec("Alpha", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let id = reg.register(spec.clone()).unwrap();

        let result = reg.get(id).unwrap();
        assert!(result.is_some());
        let (got_spec, got_status) = result.unwrap();
        assert_eq!(got_spec, spec);
        assert_eq!(got_status, ThreadStatus::Active);
    }

    #[test]
    fn registry_register_overwrites() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let mut spec = test_spec("Beta", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let id = reg.register(spec.clone()).unwrap();

        spec.charter = "Updated charter".into();
        reg.register(spec.clone()).unwrap();

        let (got_spec, _) = reg.get(id).unwrap().unwrap();
        assert_eq!(got_spec.charter, "Updated charter");
    }

    #[test]
    fn registry_list_active_filters() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let s1 = test_spec("Active1", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let s2 = test_spec(
            "Suspended",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        let s3 = test_spec("Active2", ThreadPriority::Normal, ThreadSchedule::EveryTick);

        reg.register(s1).unwrap();
        let id2 = reg.register(s2).unwrap();
        reg.register(s3).unwrap();
        reg.update_status(id2, ThreadStatus::Suspended).unwrap();

        let active = reg.list_active().unwrap();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn registry_rejects_cognitive_status_update_for_executable_thread() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);
        let spec = ThreadSpec {
            role: ThreadRole::Coding,
            flavor: ThreadFlavor::Executable,
            ..test_spec("Coding", ThreadPriority::Normal, ThreadSchedule::OnDemand)
        };

        reg.register(spec).unwrap();
        let err = reg
            .update_status(
                reg.find_registered_by_role(ThreadRole::Coding)
                    .unwrap()
                    .unwrap()
                    .0
                    .thread_id,
                ThreadStatus::Suspended,
            )
            .unwrap_err();
        assert!(err.to_string().contains("cognitive thread status"));
    }

    #[test]
    fn registry_lists_executable_threads() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);
        let spec = ThreadSpec {
            role: ThreadRole::Coding,
            flavor: ThreadFlavor::Executable,
            ..test_spec("Coding", ThreadPriority::Normal, ThreadSchedule::OnDemand)
        };

        reg.register_with_status(
            spec.clone(),
            RegisteredThreadStatus::Executable(ExecThreadStatus::Idle),
        )
        .unwrap();

        let listed = reg.list_executable().unwrap();
        assert_eq!(listed, vec![(spec, ExecThreadStatus::Idle)]);
    }
}
