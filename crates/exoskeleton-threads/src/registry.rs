//! Thread lifecycle manager.
//!
//! The ThreadRegistry manages the full lifecycle of cognitive threads:
//! registration, status transitions, scheduling, and deregistration.

use std::sync::Arc;

use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_core::{ExoError, ThreadId, ThreadOutput, ThreadSpec, ThreadStatus, ThreadSummary};

use crate::scheduling::is_thread_due;
use crate::store::ThreadStore;

/// Higher-level thread lifecycle manager wrapping a [`ThreadStore`].
///
/// Provides registration, deregistration, status transitions with terminal-
/// state guards, schedule-aware due-thread queries, and summary compilation.
pub struct ThreadRegistry {
    store: Arc<dyn ThreadStore>,
}

impl ThreadRegistry {
    /// Create a new registry backed by the given store.
    pub fn new(store: Arc<dyn ThreadStore>) -> Self {
        Self { store }
    }

    /// Register a thread specification and mark it Active.
    ///
    /// If a thread with the same ID already exists it is overwritten.
    pub fn register(&self, spec: ThreadSpec) -> Result<ThreadId, ExoError> {
        let id = spec.thread_id;
        self.store.save(&spec, ThreadStatus::Active)?;
        Ok(id)
    }

    /// Remove a thread from the registry.
    pub fn deregister(&self, thread_id: ThreadId) -> Result<(), ExoError> {
        self.store.remove(thread_id)
    }

    /// Retrieve a thread specification and its current status.
    pub fn get(&self, thread_id: ThreadId) -> Result<Option<(ThreadSpec, ThreadStatus)>, ExoError> {
        self.store.get(thread_id)
    }

    /// List all registered threads with their statuses.
    pub fn list(&self) -> Result<Vec<(ThreadSpec, ThreadStatus)>, ExoError> {
        self.store.list()
    }

    /// List only threads whose status is [`ThreadStatus::Active`].
    pub fn list_active(&self) -> Result<Vec<ThreadSpec>, ExoError> {
        let all = self.store.list()?;
        Ok(all
            .into_iter()
            .filter(|(_, status)| *status == ThreadStatus::Active)
            .map(|(spec, _)| spec)
            .collect())
    }

    /// Transition a thread to a new status.
    ///
    /// Rejects transitions from terminal states ([`ThreadStatus::Completed`]
    /// or [`ThreadStatus::Failed`]) with an [`ExoError::Engine`].
    pub fn update_status(&self, thread_id: ThreadId, status: ThreadStatus) -> Result<(), ExoError> {
        let entry = self.store.get(thread_id)?;
        match entry {
            Some((_, current_status)) => {
                if current_status.is_terminal() {
                    return Err(ExoError::Engine(format!(
                        "cannot transition from terminal state {:?}",
                        current_status
                    )));
                }
                self.store.update_status(thread_id, status)
            }
            None => Err(ExoError::Storage("thread not found".into())),
        }
    }

    /// Return threads that are due for execution on the given tick.
    ///
    /// Only Active threads are considered. Results are sorted by priority
    /// descending (Critical first).
    pub fn due_threads(&self, tick_number: u64) -> Result<Vec<ThreadSpec>, ExoError> {
        let all = self.store.list()?;
        let mut due: Vec<ThreadSpec> = all
            .into_iter()
            .filter(|(spec, status)| {
                let last_run = self.store.get_last_run(spec.thread_id).unwrap_or(None);
                is_thread_due(spec.schedule, *status, tick_number, last_run)
            })
            .map(|(spec, _)| spec)
            .collect();
        due.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(due)
    }

    /// Record the tick number of the most recent execution for a thread.
    pub fn record_run(&self, thread_id: ThreadId, tick_number: u64) -> Result<(), ExoError> {
        self.store.save_last_run(thread_id, tick_number)
    }

    /// Retrieve the most recent outputs for a thread, newest first.
    pub fn recent_outputs(
        &self,
        thread_id: ThreadId,
        limit: usize,
    ) -> Result<Vec<ThreadOutput>, ExoError> {
        self.store.recent_outputs(thread_id, limit)
    }

    /// Persist a thread output.
    pub fn save_output(&self, output: &ThreadOutput) -> Result<(), ExoError> {
        self.store.save_output(output)
    }

    /// Update the charter text of a thread in the store.
    pub fn update_charter(&self, thread_id: ThreadId, charter: String) -> Result<(), ExoError> {
        self.store.update_charter(thread_id, charter)
    }

    /// Reload thread charters from the prompt registry.
    ///
    /// For each registered thread, checks if a charter override exists in the
    /// registry (keyed by `charter-{name-slug}`). If the charter text differs
    /// from the current value, updates it in the store.
    ///
    /// Returns the number of charters updated.
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

    /// Build a [`ThreadSummary`] for every registered thread.
    ///
    /// The `token_budget_remaining` field is set to the thread's static
    /// `token_budget` as a placeholder until per-tick budget tracking is
    /// implemented (Sprint 9).
    pub fn thread_summaries(&self) -> Result<Vec<ThreadSummary>, ExoError> {
        let all = self.store.list()?;
        let mut summaries = Vec::with_capacity(all.len());
        for (spec, status) in all {
            let last_output_summary = self
                .store
                .recent_outputs(spec.thread_id, 1)?
                .first()
                .map(|o| o.summary.clone());
            summaries.push(ThreadSummary {
                thread_id: spec.thread_id,
                name: spec.name.clone(),
                status,
                last_output_summary,
                token_budget_remaining: spec.token_budget,
            });
        }
        Ok(summaries)
    }
}

/// Convert a thread name to its charter registry key.
/// "Threat Monitor" -> "charter-threat-monitor"
fn charter_key(name: &str) -> String {
    format!("charter-{}", name.to_lowercase().replace(' ', "-"))
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{ThreadId, ThreadPriority, ThreadSchedule, ThreadSpec, ThreadStatus};

    use super::*;
    use crate::store::InMemoryThreadStore;

    fn test_spec(name: &str, priority: ThreadPriority, schedule: ThreadSchedule) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            name: name.into(),
            charter: format!("Test charter for {name}"),
            priority,
            token_budget: 2000,
            schedule,
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

        // Re-register with same ID but different charter.
        spec.charter = "Updated charter".into();
        reg.register(spec.clone()).unwrap();

        let (got_spec, _) = reg.get(id).unwrap().unwrap();
        assert_eq!(got_spec.charter, "Updated charter");
    }

    #[test]
    fn registry_deregister() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec("Gamma", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let id = reg.register(spec).unwrap();
        reg.deregister(id).unwrap();

        assert!(reg.get(id).unwrap().is_none());
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

        // Suspend the second thread.
        reg.update_status(id2, ThreadStatus::Suspended).unwrap();

        let active = reg.list_active().unwrap();
        assert_eq!(active.len(), 2);
        assert!(active.iter().all(|s| s.name != "Suspended"));
    }

    #[test]
    fn registry_update_status_transition() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec("Delta", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let id = reg.register(spec).unwrap();

        reg.update_status(id, ThreadStatus::Suspended).unwrap();
        let (_, status) = reg.get(id).unwrap().unwrap();
        assert_eq!(status, ThreadStatus::Suspended);
    }

    #[test]
    fn registry_update_status_terminal_rejected() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec("Epsilon", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let id = reg.register(spec).unwrap();

        // Transition to Completed (terminal).
        reg.update_status(id, ThreadStatus::Completed).unwrap();

        // Attempting to transition from Completed should fail.
        let result = reg.update_status(id, ThreadStatus::Active);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cannot transition from terminal state"),
            "Expected terminal-state error, got: {err_msg}"
        );
    }

    #[test]
    fn registry_due_threads_every_tick() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec(
            "EveryTicker",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        reg.register(spec.clone()).unwrap();

        let due = reg.due_threads(0).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "EveryTicker");

        let due = reg.due_threads(42).unwrap();
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn registry_due_threads_every_n_ticks() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec(
            "Periodic",
            ThreadPriority::Normal,
            ThreadSchedule::EveryNTicks(3),
        );
        let id = reg.register(spec).unwrap();

        // Never run -> due on tick 1.
        let due = reg.due_threads(1).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "Periodic");

        // Record run at tick 1.
        reg.record_run(id, 1).unwrap();

        // Tick 2: elapsed=1 < 3, not due.
        assert!(reg.due_threads(2).unwrap().is_empty());

        // Tick 3: elapsed=2 < 3, not due.
        assert!(reg.due_threads(3).unwrap().is_empty());

        // Tick 4: elapsed=3 >= 3, due.
        let due = reg.due_threads(4).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "Periodic");

        // Record run at tick 4, check tick 7 is due.
        reg.record_run(id, 4).unwrap();
        let due = reg.due_threads(7).unwrap();
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn registry_due_threads_on_demand_never_auto() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec(
            "OnDemandThread",
            ThreadPriority::Normal,
            ThreadSchedule::OnDemand,
        );
        reg.register(spec).unwrap();

        // OnDemand threads should never be returned by due_threads.
        for tick in 0..10 {
            assert!(
                reg.due_threads(tick).unwrap().is_empty(),
                "OnDemand should not be due on tick {tick}"
            );
        }
    }

    #[test]
    fn registry_due_threads_sorted_by_priority() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let background = test_spec("BG", ThreadPriority::Background, ThreadSchedule::EveryTick);
        let normal = test_spec("NRM", ThreadPriority::Normal, ThreadSchedule::EveryTick);
        let critical = test_spec("CRIT", ThreadPriority::Critical, ThreadSchedule::EveryTick);

        // Register in non-priority order.
        reg.register(background).unwrap();
        reg.register(normal).unwrap();
        reg.register(critical).unwrap();

        let due = reg.due_threads(1).unwrap();
        assert_eq!(due.len(), 3);
        assert_eq!(due[0].name, "CRIT");
        assert_eq!(due[1].name, "NRM");
        assert_eq!(due[2].name, "BG");
    }

    // ── E0-T20: reload_charters updates from registry ──

    #[test]
    fn reload_charters_updates_from_registry() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        // Register threads with original charters
        let spec = test_spec(
            "Threat Monitor",
            ThreadPriority::Critical,
            ThreadSchedule::EveryTick,
        );
        let id = reg.register(spec).unwrap();

        // Build a PromptRegistry with a different charter
        let mut prompts = PromptRegistry::new();
        prompts.insert("charter-threat-monitor", "Updated threat charter text");

        let updated = reg.reload_charters(&prompts).unwrap();
        assert_eq!(updated, 1, "should update 1 charter");

        let (got_spec, _) = reg.get(id).unwrap().unwrap();
        assert_eq!(got_spec.charter, "Updated threat charter text");
    }

    #[test]
    fn reload_charters_skips_unchanged() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec(
            "Self-Critique",
            ThreadPriority::High,
            ThreadSchedule::EveryTick,
        );
        reg.register(spec.clone()).unwrap();

        // Registry has the same charter text
        let mut prompts = PromptRegistry::new();
        prompts.insert("charter-self-critique", &spec.charter);

        let updated = reg.reload_charters(&prompts).unwrap();
        assert_eq!(updated, 0, "should not update when charter is unchanged");
    }

    #[test]
    fn reload_charters_ignores_missing_keys() {
        let store = Arc::new(InMemoryThreadStore::new());
        let reg = ThreadRegistry::new(store);

        let spec = test_spec(
            "Custom Thread",
            ThreadPriority::Normal,
            ThreadSchedule::EveryTick,
        );
        reg.register(spec).unwrap();

        // Empty registry — no charter keys
        let prompts = PromptRegistry::new();

        let updated = reg.reload_charters(&prompts).unwrap();
        assert_eq!(updated, 0, "should not update when key is missing");
    }
}
