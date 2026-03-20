//! TypeScript type codegen binary.                                                                                                                                                                                                                                                                                                                                                         
//!                                                       
//! Generates a single file of `export type ...` declarations from all
//! Rust types annotated with `#[derive(ts_rs::TS)]` across exoskeleton crates.                                                                                                                                                                                                                                                                                                             
//!                                                                                                                                                                                                                                                                                                                                                                                         
//! Usage:                                                                                                                                                                                                                                                                                                                                                                                  
//!   cargo run -p exoskeleton-daemon --bin export-types -- `<output-path>`                                                                                                                                                                                                                                                                                                                   
//!   cargo run -p exoskeleton-daemon --bin export-types          # writes to stdout                                                                                                                                                                                                                                                                                                        

use std::io::Write;

// ── exoskeleton-core types ──
use exoskeleton_core::artifact::{Artifact, ArtifactKind, ArtifactRef};
use exoskeleton_core::budget::{
    CognitiveBudgetConfig, EscalationPolicy, ThrashLevel, ToolBudgetConfig,
};
use exoskeleton_core::conversation::{
    Conversation, ConversationMessage, ConversationMessageWithContent, ConversationState,
};
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

fn generate() -> String {
    let cfg = ts_rs::Config::new().with_large_int("number");
    let mut output = String::new();
    output.push_str(
        "// Auto-generated from Rust types via ts-rs. Do not edit manually.\n\
         // Regenerate with: cargo run -p exoskeleton-daemon --bin export-types -- <output-path>\n\n",
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

    // ── Config types (E1-S3) ──
    emit!(output, &cfg, exoskeleton_core::TrustDecayConfig);

    // ── Conversation types (E1-S2, OA-S2) ──
    emit!(output, &cfg, ConversationMessage);
    emit!(output, &cfg, ConversationMessageWithContent);
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
    output
}

fn main() {
    let output = generate();

    match std::env::args().nth(1) {
        Some(path) => {
            let path = std::path::Path::new(&path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("failed to create output directory");
            }
            std::fs::write(path, &output).expect("failed to write output file");
            eprintln!("Wrote {} bytes to {}", output.len(), path.display());
        }
        None => {
            std::io::stdout().write_all(output.as_bytes()).unwrap();
        }
    }
}
