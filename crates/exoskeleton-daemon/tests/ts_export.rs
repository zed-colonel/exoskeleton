//! TypeScript type generation test (E3-S2, W-4).
//!
//! Generates `observatory/src/api/types.generated.ts` from Rust types
//! annotated with `#[derive(ts_rs::TS)]` across all crates.
//!
//! Run with: cargo test -p exoskeleton-daemon --no-default-features export_typescript_types

// ── exoskeleton-core types ──
use exoskeleton_core::artifact::{Artifact, ArtifactKind, ArtifactRef};
use exoskeleton_core::budget::{
    CognitiveBudgetConfig, EscalationPolicy, ThrashLevel, ToolBudgetConfig,
};
use exoskeleton_core::conversation::{Conversation, ConversationMessage, ConversationState};
use exoskeleton_core::event::{EventEntry, EventType, LiveEvent};
use exoskeleton_core::memory::{EpisodicSummary, LongTermNote};
use exoskeleton_core::plan::{Plan, PlanTask, PlanTaskStatus};
use exoskeleton_core::relationship::{
    PrincipalSummary, RelationalSignalType, RelationshipRecord, RelationshipSnapshot,
};
use exoskeleton_core::snapshot::{BudgetStatus, StateSnapshot, ThreadSummary, VesselStatus};
use exoskeleton_core::thread::{ThreadPriority, ThreadSchedule, ThreadStatus};
use exoskeleton_core::tick::{
    ActionOutcome, ActionRecord, LlmCallRecord, ThreadContribution, TickPhase, TickRecord,
};
use exoskeleton_core::working_memory::{WorkingMemory, WorkingMemoryEntry};
use exoskeleton_core::{
    ArtifactId, ConversationId, EnvelopeId, LedgerEntryId, PlanTaskId, PrincipalId, ThreadId,
    TickId, VesselId,
};
// ── exoskeleton-daemon types ──
use exoskeleton_daemon::handlers::{
    ForkResponse, MemoryResponse, SanitizedConfig, SanitizedFrontierConfig, SanitizedLocalConfig,
};
// ── exoskeleton-host types ──
use exoskeleton_host::inspect::{
    CognitiveBudgetDetail, CognitiveEngineStatus, EngineStatus, InboxHistoryEntry,
    InspectionBudgetStatus, ThreadStatusEntry, ToolBudgetDetail, ToolEngineStatus,
};
// ── exoskeleton-memory types ──
use exoskeleton_memory::compiler::{CompiledContext, SectionResult};
use ts_rs::TS;

/// Collect a TS type declaration, appending it to the output buffer with `export`.
macro_rules! emit {
    ($out:expr, $cfg:expr, $ty:ty) => {
        $out.push_str("export ");
        $out.push_str(&<$ty>::decl($cfg));
        $out.push_str("\n\n");
    };
}

#[test]
fn export_typescript_types() {
    let cfg = ts_rs::Config::new().with_large_int("number");
    let mut output = String::new();
    output.push_str(
        "// Auto-generated from Rust types via ts-rs. Do not edit manually.\n\
         // Regenerate with: cargo test -p exoskeleton-daemon --no-default-features export_typescript_types\n\n",
    );

    // ── ID newtypes (leaf types) ──
    emit!(output, &cfg, VesselId);
    emit!(output, &cfg, TickId);
    emit!(output, &cfg, ThreadId);
    emit!(output, &cfg, PrincipalId);
    emit!(output, &cfg, EnvelopeId);
    emit!(output, &cfg, LedgerEntryId);
    emit!(output, &cfg, ArtifactId);
    emit!(output, &cfg, PlanTaskId);
    emit!(output, &cfg, ConversationId);

    // ── Simple enums ──
    emit!(output, &cfg, VesselStatus);
    emit!(output, &cfg, TickPhase);
    emit!(output, &cfg, ActionOutcome);
    emit!(output, &cfg, EventType);
    emit!(output, &cfg, ArtifactKind);
    emit!(output, &cfg, RelationalSignalType);
    emit!(output, &cfg, ThreadPriority);
    emit!(output, &cfg, ThreadSchedule);
    emit!(output, &cfg, ThreadStatus);
    emit!(output, &cfg, ThrashLevel);
    emit!(output, &cfg, PlanTaskStatus);
    emit!(output, &cfg, ConversationState);

    // ── Plan & Working Memory types (E1-S1) ──
    emit!(output, &cfg, Plan);
    emit!(output, &cfg, PlanTask);
    emit!(output, &cfg, WorkingMemory);
    emit!(output, &cfg, WorkingMemoryEntry);

    // ── Core structs ──
    emit!(output, &cfg, BudgetStatus);
    emit!(output, &cfg, ThreadSummary);
    emit!(output, &cfg, StateSnapshot);
    emit!(output, &cfg, ThreadContribution);
    emit!(output, &cfg, ActionRecord);
    emit!(output, &cfg, LlmCallRecord);
    emit!(output, &cfg, TickRecord);
    emit!(output, &cfg, EventEntry);
    emit!(output, &cfg, LiveEvent);
    emit!(output, &cfg, Artifact);
    emit!(output, &cfg, ArtifactRef);
    emit!(output, &cfg, PrincipalSummary);
    emit!(output, &cfg, RelationshipSnapshot);
    emit!(output, &cfg, RelationshipRecord);
    emit!(output, &cfg, EpisodicSummary);
    emit!(output, &cfg, LongTermNote);
    emit!(output, &cfg, CognitiveBudgetConfig);
    emit!(output, &cfg, ToolBudgetConfig);
    emit!(output, &cfg, EscalationPolicy);

    // ── Conversation types (E1-S2) ──
    emit!(output, &cfg, ConversationMessage);
    emit!(output, &cfg, Conversation);

    // ── Memory compiler types (W-24) ──
    emit!(output, &cfg, SectionResult);
    emit!(output, &cfg, CompiledContext);

    // ── Host inspect types ──
    emit!(output, &cfg, CognitiveBudgetDetail);
    emit!(output, &cfg, ToolBudgetDetail);
    emit!(output, &cfg, CognitiveEngineStatus);
    emit!(output, &cfg, ToolEngineStatus);
    emit!(output, &cfg, EngineStatus);
    emit!(output, &cfg, InspectionBudgetStatus);
    emit!(output, &cfg, ThreadStatusEntry);
    emit!(output, &cfg, InboxHistoryEntry);

    // ── Daemon handler types ──
    emit!(output, &cfg, SanitizedLocalConfig);
    emit!(output, &cfg, SanitizedFrontierConfig);
    emit!(output, &cfg, SanitizedConfig);
    emit!(output, &cfg, MemoryResponse);
    emit!(output, &cfg, ForkResponse);

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../observatory/src/api/types.generated.ts");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create output directory");
    }
    std::fs::write(&path, &output).expect("failed to write types.generated.ts");

    // Verify the file was written
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("VesselId"), "Missing VesselId type");
    assert!(
        content.contains("StateSnapshot"),
        "Missing StateSnapshot type"
    );
    assert!(content.contains("TickRecord"), "Missing TickRecord type");
    assert!(
        content.contains("CompiledContext"),
        "Missing CompiledContext type"
    );
    assert!(
        content.contains("SanitizedConfig"),
        "Missing SanitizedConfig type"
    );
    assert!(
        content.contains("ForkResponse"),
        "Missing ForkResponse type"
    );
    assert!(
        content.contains("Conversation"),
        "Missing Conversation type"
    );
    assert!(
        content.contains("ConversationState"),
        "Missing ConversationState type"
    );
}

// ── E3-T14: ID newtypes generate as string aliases ──

#[test]
fn ts_id_newtypes_are_string_aliases() {
    let cfg = ts_rs::Config::default();
    assert_eq!(VesselId::decl(&cfg), "type VesselId = string;");
    assert_eq!(TickId::decl(&cfg), "type TickId = string;");
    assert_eq!(ThreadId::decl(&cfg), "type ThreadId = string;");
    assert_eq!(PrincipalId::decl(&cfg), "type PrincipalId = string;");
    assert_eq!(EnvelopeId::decl(&cfg), "type EnvelopeId = string;");
    assert_eq!(LedgerEntryId::decl(&cfg), "type LedgerEntryId = string;");
    assert_eq!(ArtifactId::decl(&cfg), "type ArtifactId = string;");
}

// ── E3-T15: ThreadSchedule enum with mixed variants ──

#[test]
fn ts_thread_schedule_mixed_variants() {
    let cfg = ts_rs::Config::default();
    let decl = ThreadSchedule::decl(&cfg);
    assert!(decl.contains("every_tick"), "ThreadSchedule decl: {decl}");
    assert!(decl.contains("on_demand"), "ThreadSchedule decl: {decl}");
    assert!(
        decl.contains("every_n_ticks"),
        "ThreadSchedule decl: {decl}"
    );
}
