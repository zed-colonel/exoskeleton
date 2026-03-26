//! Thread registry, sub-workflow execution, and context slicing for Exoskeleton.
//!
//! This crate provides:
//!
//! - [`ThreadStore`] trait and [`InMemoryThreadStore`] for thread persistence.
//! - [`is_thread_due`] pure scheduling function.
//! - [`ThreadRegistry`] higher-level lifecycle manager.
//! - [`compile_thread_context`] token-budgeted context slicing for threads.

pub mod builtin;
pub mod context;
pub mod registry;
pub mod scheduling;
pub mod store;

pub use builtin::{
    register_builtin_threads, CharterProposalDraft, CognitivePattern, CreativeSynthesis,
    Hypothesis, MemoryConsolidation, MemoryNote, MetaCognitionAnalysis, NovelConnection,
    PatternSeverity, SelfCritique, ThreadConfigOverrides, Threat, ThreatAssessment, ThreatSeverity,
    WatchSuggestion, CREATIVE_SYNTHESIS_ID, MEMORY_CONSOLIDATION_ID, META_COGNITION_ID,
    SELF_CRITIQUE_ID, THREAT_MONITOR_ID,
};
pub use context::compile_thread_context;
pub use registry::ThreadRegistry;
pub use scheduling::is_thread_due;
use serde::{Deserialize, Serialize};
pub use store::{InMemoryThreadStore, ThreadStore};

/// Structured response from a cognitive thread's LLM call.
///
/// Threads produce analysis and recommendations — never actions.
/// This is the JSON format the thread's system prompt instructs the LLM
/// to respond with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResponse {
    /// One-sentence summary of the thread's analysis.
    pub summary: String,
    /// Specific recommendations for the master loop's Decide step.
    #[serde(default)]
    pub recommendations: Vec<String>,
}
