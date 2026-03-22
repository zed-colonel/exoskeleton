//! Built-in cognitive thread definitions.
//!
//! Three foundational threads ship with every Exoskeleton vessel:
//! - **Threat Monitor** (Critical, EveryTick): safety and alignment scanning
//! - **Self-Critique** (High, EveryTick): decision quality evaluation
//! - **Memory Consolidation** (Normal, EveryNTicks(5)): experience consolidation

pub mod memory_consolidation;
pub mod self_critique;
pub mod threat_monitor;

use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_core::{ExoError, ThreadId};
pub use memory_consolidation::{MemoryConsolidation, MemoryNote};
pub use self_critique::SelfCritique;
pub use threat_monitor::{Threat, ThreatAssessment, ThreatSeverity};
use uuid::Uuid;

use exoskeleton_core::ThreadSchedule;

use crate::registry::ThreadRegistry;

/// Operator overrides for built-in thread configuration.
///
/// Applied after charter overrides, before registration. Allows operators
/// to tune built-in thread behavior via `vessel.toml` `[threads]` section.
#[derive(Debug, Default)]
pub struct ThreadConfigOverrides {
    /// Threat Monitor schedule override.
    pub threat_monitor_schedule: Option<ThreadSchedule>,
    /// Threat Monitor token budget override.
    pub threat_monitor_token_budget: Option<u64>,
    /// Self-Critique schedule override.
    pub self_critique_schedule: Option<ThreadSchedule>,
    /// Self-Critique token budget override.
    pub self_critique_token_budget: Option<u64>,
    /// Memory Consolidation schedule override.
    pub memory_consolidation_schedule: Option<ThreadSchedule>,
    /// Memory Consolidation token budget override.
    pub memory_consolidation_token_budget: Option<u64>,
}

/// Deterministic UUID for the Threat Monitor thread.
///
/// Hardcoded so the same thread is recognized across restarts
/// and re-registrations without duplication.
pub const THREAT_MONITOR_ID: ThreadId = ThreadId::from_uuid(Uuid::from_bytes([
    0xca, 0xe1, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]));

/// Deterministic UUID for the Self-Critique thread.
pub const SELF_CRITIQUE_ID: ThreadId = ThreadId::from_uuid(Uuid::from_bytes([
    0xca, 0xe1, 0x00, 0x02, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
]));

/// Deterministic UUID for the Memory Consolidation thread.
pub const MEMORY_CONSOLIDATION_ID: ThreadId = ThreadId::from_uuid(Uuid::from_bytes([
    0xca, 0xe1, 0x00, 0x03, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
]));

/// Register all built-in threads if they are not already present.
///
/// Checks each built-in thread by its deterministic ID. If a thread with
/// that ID already exists (in any status, including Suspended or Failed),
/// it is NOT re-registered — preserving operator status changes.
///
/// Charter text comes from the `PromptRegistry` (Epoch 0). If the registry
/// has a charter entry for a thread, it overrides the compiled-in default.
pub fn register_builtin_threads(
    registry: &ThreadRegistry,
    prompts: &PromptRegistry,
    overrides: Option<&ThreadConfigOverrides>,
) -> Result<(), ExoError> {
    let mut builtin_specs = [
        threat_monitor::spec(),
        self_critique::spec(),
        memory_consolidation::spec(),
    ];

    // Override charters from prompt registry (Epoch 0)
    let charter_keys = [
        "charter-threat-monitor",
        "charter-self-critique",
        "charter-memory-consolidation",
    ];
    for (spec, key) in builtin_specs.iter_mut().zip(charter_keys.iter()) {
        if let Some(charter) = prompts.get(key) {
            spec.charter = charter.to_string();
        }
    }

    // Apply operator overrides for schedule and token budget
    if let Some(o) = overrides {
        // Threat Monitor (index 0)
        if let Some(schedule) = o.threat_monitor_schedule {
            builtin_specs[0].schedule = schedule;
        }
        if let Some(budget) = o.threat_monitor_token_budget {
            builtin_specs[0].token_budget = budget;
        }
        // Self-Critique (index 1)
        if let Some(schedule) = o.self_critique_schedule {
            builtin_specs[1].schedule = schedule;
        }
        if let Some(budget) = o.self_critique_token_budget {
            builtin_specs[1].token_budget = budget;
        }
        // Memory Consolidation (index 2)
        if let Some(schedule) = o.memory_consolidation_schedule {
            builtin_specs[2].schedule = schedule;
        }
        if let Some(budget) = o.memory_consolidation_token_budget {
            builtin_specs[2].token_budget = budget;
        }
    }

    for spec in &builtin_specs {
        match registry.get(spec.thread_id)? {
            Some(_) => {
                tracing::debug!(
                    thread_name = %spec.name,
                    thread_id = %spec.thread_id,
                    "built-in thread already registered, skipping"
                );
            }
            None => {
                registry.register(spec.clone())?;
                tracing::info!(
                    thread_name = %spec.name,
                    thread_id = %spec.thread_id,
                    "registered built-in thread"
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::ThreadStatus;

    use super::*;
    use crate::store::InMemoryThreadStore;

    #[test]
    fn deterministic_ids_are_stable() {
        // IDs must be the same across invocations.
        assert_eq!(THREAT_MONITOR_ID, threat_monitor::spec().thread_id);
        assert_eq!(SELF_CRITIQUE_ID, self_critique::spec().thread_id);
        assert_eq!(
            MEMORY_CONSOLIDATION_ID,
            memory_consolidation::spec().thread_id
        );
    }

    #[test]
    fn deterministic_ids_are_distinct() {
        assert_ne!(THREAT_MONITOR_ID, SELF_CRITIQUE_ID);
        assert_ne!(SELF_CRITIQUE_ID, MEMORY_CONSOLIDATION_ID);
        assert_ne!(THREAT_MONITOR_ID, MEMORY_CONSOLIDATION_ID);
    }

    #[test]
    fn register_builtin_threads_creates_all_three() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);

        register_builtin_threads(&registry, &PromptRegistry::with_defaults(), None).unwrap();

        let all = registry.list().unwrap();
        assert_eq!(all.len(), 3);
        let names: Vec<&str> = all.iter().map(|(s, _)| s.name.as_str()).collect();
        assert!(names.contains(&"Threat Monitor"));
        assert!(names.contains(&"Self-Critique"));
        assert!(names.contains(&"Memory Consolidation"));
    }

    #[test]
    fn register_builtin_threads_is_idempotent() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);

        register_builtin_threads(&registry, &PromptRegistry::with_defaults(), None).unwrap();
        register_builtin_threads(&registry, &PromptRegistry::with_defaults(), None).unwrap();

        let all = registry.list().unwrap();
        assert_eq!(
            all.len(),
            3,
            "should not duplicate threads on re-registration"
        );
    }

    #[test]
    fn register_preserves_operator_status_changes() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);

        register_builtin_threads(&registry, &PromptRegistry::with_defaults(), None).unwrap();

        // Operator suspends the Threat Monitor.
        registry
            .update_status(THREAT_MONITOR_ID, ThreadStatus::Suspended)
            .unwrap();

        // Re-register (e.g., vessel restart).
        register_builtin_threads(&registry, &PromptRegistry::with_defaults(), None).unwrap();

        // Threat Monitor should still be Suspended.
        let (_, status) = registry.get(THREAT_MONITOR_ID).unwrap().unwrap();
        assert_eq!(status, ThreadStatus::Suspended);

        // Total count unchanged.
        assert_eq!(registry.list().unwrap().len(), 3);
    }

    // ── DC-T11..DC-T13: Decoherence Fix — Thread Config Overrides ──

    #[test]
    fn dc_t11_thread_config_overrides_applied() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);
        let overrides = ThreadConfigOverrides {
            threat_monitor_schedule: Some(ThreadSchedule::EveryNTicks(3)),
            threat_monitor_token_budget: Some(2048),
            self_critique_schedule: Some(ThreadSchedule::OnDemand),
            self_critique_token_budget: Some(3000),
            memory_consolidation_schedule: Some(ThreadSchedule::EveryNTicks(10)),
            memory_consolidation_token_budget: Some(8000),
        };

        register_builtin_threads(
            &registry,
            &PromptRegistry::with_defaults(),
            Some(&overrides),
        )
        .unwrap();

        let (tm, _) = registry.get(THREAT_MONITOR_ID).unwrap().unwrap();
        assert_eq!(tm.schedule, ThreadSchedule::EveryNTicks(3));
        assert_eq!(tm.token_budget, 2048);

        let (sc, _) = registry.get(SELF_CRITIQUE_ID).unwrap().unwrap();
        assert_eq!(sc.schedule, ThreadSchedule::OnDemand);
        assert_eq!(sc.token_budget, 3000);

        let (mc, _) = registry.get(MEMORY_CONSOLIDATION_ID).unwrap().unwrap();
        assert_eq!(mc.schedule, ThreadSchedule::EveryNTicks(10));
        assert_eq!(mc.token_budget, 8000);
    }

    #[test]
    fn dc_t12_thread_config_overrides_none_preserves_defaults() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);

        register_builtin_threads(&registry, &PromptRegistry::with_defaults(), None).unwrap();

        let (tm, _) = registry.get(THREAT_MONITOR_ID).unwrap().unwrap();
        assert_eq!(tm.schedule, ThreadSchedule::EveryTick);
        assert_eq!(tm.token_budget, 4096);

        let (sc, _) = registry.get(SELF_CRITIQUE_ID).unwrap().unwrap();
        assert_eq!(sc.schedule, ThreadSchedule::EveryTick);
        assert_eq!(sc.token_budget, 4096);
    }

    #[test]
    fn dc_t13_register_with_overrides_idempotent() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);
        let overrides = ThreadConfigOverrides {
            threat_monitor_schedule: Some(ThreadSchedule::EveryNTicks(5)),
            ..Default::default()
        };

        register_builtin_threads(
            &registry,
            &PromptRegistry::with_defaults(),
            Some(&overrides),
        )
        .unwrap();
        register_builtin_threads(
            &registry,
            &PromptRegistry::with_defaults(),
            Some(&overrides),
        )
        .unwrap();

        assert_eq!(
            registry.list().unwrap().len(),
            3,
            "should not duplicate threads on re-registration with overrides"
        );
    }
}
