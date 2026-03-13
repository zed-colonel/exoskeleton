//! Built-in cognitive thread definitions.
//!
//! Three foundational threads ship with every Exoskeleton vessel:
//! - **Threat Monitor** (Critical, EveryTick): safety and alignment scanning
//! - **Self-Critique** (High, EveryTick): decision quality evaluation
//! - **Memory Consolidation** (Normal, EveryNTicks(5)): experience consolidation

pub mod memory_consolidation;
pub mod self_critique;
pub mod threat_monitor;

use exoskeleton_core::{ExoError, ThreadId};
pub use memory_consolidation::{MemoryConsolidation, MemoryNote};
pub use self_critique::SelfCritique;
pub use threat_monitor::{Threat, ThreatAssessment, ThreatSeverity};
use uuid::Uuid;

use crate::registry::ThreadRegistry;

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
pub fn register_builtin_threads(registry: &ThreadRegistry) -> Result<(), ExoError> {
    let builtin_specs = [
        threat_monitor::spec(),
        self_critique::spec(),
        memory_consolidation::spec(),
    ];

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

        register_builtin_threads(&registry).unwrap();

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

        register_builtin_threads(&registry).unwrap();
        register_builtin_threads(&registry).unwrap();

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

        register_builtin_threads(&registry).unwrap();

        // Operator suspends the Threat Monitor.
        registry
            .update_status(THREAT_MONITOR_ID, ThreadStatus::Suspended)
            .unwrap();

        // Re-register (e.g., vessel restart).
        register_builtin_threads(&registry).unwrap();

        // Threat Monitor should still be Suspended.
        let (_, status) = registry.get(THREAT_MONITOR_ID).unwrap().unwrap();
        assert_eq!(status, ThreadStatus::Suspended);

        // Total count unchanged.
        assert_eq!(registry.list().unwrap().len(), 3);
    }
}
