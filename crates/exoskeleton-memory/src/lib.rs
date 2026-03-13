//! Context Compiler + episodic/long-term memory tiers for Exoskeleton.
//!
//! This crate provides the cognitive infrastructure for I5 (context compiled,
//! not accumulated):
//!
//! - **Token counting** (`tokens`): `TokenCounter` trait + `ApproximateTokenCounter`
//! - **Memory store** (`store`): `MemoryStore` trait for episodic/long-term persistence
//! - **Section renderers** (`render`): pure functions that format domain types as text
//! - **Context compiler** (`compiler`): budget-aware prompt assembly from multiple sources
//!
//! This crate has zero I/O dependencies — no `rusqlite`, no network, no file system.
//! The SQLite implementation of `MemoryStore` lives in `exoskeleton-host`.

pub mod compiler;
pub mod render;
pub mod store;
pub mod tokens;

pub use compiler::{
    CompiledContext, ContextCompiler, ContextSources, SectionAllocation, SectionPriorities,
    SectionPriority, SectionResult,
};
pub use store::MemoryStore;
pub use tokens::{approximate_token_count, ApproximateTokenCounter, TokenCounter};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use exoskeleton_core::{StateSnapshot, VesselId};
    use proptest::prelude::*;

    use super::*;

    // ── T-10: Property-Based Tests ──

    proptest! {
        #[test]
        fn token_count_is_deterministic(ref s in "\\PC{0,500}") {
            let counter = ApproximateTokenCounter;
            let a = counter.count(s);
            let b = counter.count(s);
            prop_assert_eq!(a, b);
        }

        #[test]
        fn truncate_respects_budget(ref s in "\\PC{0,500}", budget in 0u64..200) {
            let counter = ApproximateTokenCounter;
            let truncated = counter.truncate_to_budget(s, budget);
            let count = counter.count(&truncated);
            prop_assert!(
                count <= budget,
                "count={count} exceeds budget={budget} for truncated text len={}",
                truncated.len()
            );
        }

        #[test]
        fn compiled_context_within_budget(budget in 50u64..5000) {
            let counter = Arc::new(ApproximateTokenCounter);
            let compiler = ContextCompiler::with_defaults(counter, budget);

            let snap = StateSnapshot::initial(VesselId::new(), "prop test".into());
            let sources = ContextSources {
                vessel_id: snap.vessel_id,
                mission: &snap.mission,
                snapshot: &snap,
                relationship_snapshot: None,
                thread_contributions: &[],
                recent_events: &[],
                episodic_summaries: &[],
                long_term_notes: &[],
                working_context: "",
            };

            match compiler.compile(&sources) {
                Ok(result) => {
                    prop_assert!(
                        result.total_tokens <= budget,
                        "total_tokens={} exceeds budget={budget}",
                        result.total_tokens
                    );
                }
                Err(_) => {
                    // Budget too small for critical sections — expected for very small budgets
                }
            }
        }
    }
}
