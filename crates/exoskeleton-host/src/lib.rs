#![forbid(unsafe_code)]
//! Vessel runtime: dual-engine bootstrap, master loop kernel, and LLM handler.
//!
//! The Vessel owns two independent ActionQueue engines (I9):
//! - **Cognitive AQ** (direct): master loop ticks, thread executions, LLM inference
//! - **Tool AQ** (via WI Host): adapter invocations, workflows, external I/O
//!
//! The Act step is the sole boundary crossing from cognitive decisions to tool
//! execution. LLM calls are cognitive work dispatched through the Cognitive AQ,
//! not WI adapters. Threads never invoke tools — they produce recommendation
//! artifacts only.

pub mod benchmark;
pub mod budget;
pub mod cognitive_engine;
pub mod config;
pub mod inbox;
pub mod inspect;
pub mod introspection;
pub mod kernel;
pub mod llm;
pub mod metrics;
pub mod prompt_loader;
pub mod storage;
pub mod swe_bench;
pub mod vessel;

pub use cognitive_engine::{
    bootstrap_cognitive_engine_with_backends, CognitiveHandler, CognitivePayload, CognitiveTaskType,
};
pub use config::{
    CodingDefaults, FrontierModelConfig, FrontierProvider, LlmConfig, LocalApiFormat,
    LocalModelConfig, VesselConfig, VesselConfigFile,
};
pub use inbox::{FileInbox, InMemoryInbox};
pub use inspect::VesselInspector;
pub use kernel::{KernelContext, WiHostSlot};
pub use llm::client::LlmClient;
pub use llm::direct::direct_llm_call;
pub use llm::http::LlmHttpBackend;
pub use llm::mock::{
    default_mock_response, mock_end_turn_response, mock_text_and_tool_use_response,
    mock_tool_use_response, MockLlmBackend, MockSequenceLlmBackend,
};
pub use metrics::ExoMetrics;
pub use storage::{
    SqliteArtifactStore, SqliteBudgetStore, SqliteEventLedger, SqliteMemoryStore,
    SqliteSnapshotStore, SqliteThreadStore, SqliteTickStore, StorageManager,
};
pub use swe_bench::{
    DatasetSource, SweBenchRunner, SweGrading, SweInstance, SweResult, SweSuiteResult,
    SweRunOptions,
};
pub use vessel::{CognitiveEngineSlot, Vessel};
