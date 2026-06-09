//! Host-agnostic runtime contracts for thread execution.

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::conversation::Conversation;
use exoskeleton_core::llm::LlmRequest;
use exoskeleton_core::tick::{ExecThreadContribution, ThreadContribution};
use exoskeleton_core::{
    ActionOutcome, Artifact, ArtifactId, EventEntry, ExoError, StateSnapshot, ThreadId, TickId,
};

#[derive(Debug, Clone)]
pub struct ThreadLlmResponse {
    pub text: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Debug, Clone)]
pub struct ExecutableThreadPerception {
    pub new_messages: Vec<exoskeleton_core::MessageEnvelope>,
    pub active_conversations: Vec<Conversation>,
    pub thread_outputs: Vec<ThreadContribution>,
    pub exec_thread_outputs: Vec<ExecThreadContribution>,
    pub pending_action_results: Vec<EventEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct CodingActionFeedback {
    pub mutating_success: bool,
    pub mutating_failure: bool,
    pub verification_success: bool,
    pub verification_failure: bool,
    pub semantic_resolution: Option<SemanticActionFeedback>,
}

#[derive(Debug, Clone)]
pub struct SemanticActionFeedback {
    pub tool_name: String,
    pub query: Option<String>,
    pub symbol_id: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub file_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

pub trait ThreadRuntime {
    fn prompt(&self, key: &str) -> Option<String>;
    fn put_artifact(&self, artifact: &Artifact) -> Result<ArtifactId, ExoError>;
    fn get_artifact_text(&self, artifact_id: &ArtifactId) -> Result<Option<String>, ExoError>;
    fn llm_call(
        &self,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<ThreadLlmResponse, ExoError>;
    fn latest_coding_feedback(&self, thread_id: ThreadId)
        -> Result<CodingActionFeedback, ExoError>;
}

pub struct ExecutableThreadContext<'a> {
    pub spec: &'a exoskeleton_core::ThreadSpec,
    pub snapshot: &'a StateSnapshot,
    pub perception: &'a ExecutableThreadPerception,
    pub tick_id: TickId,
}

pub fn coding_feedback_from_actions<'a>(
    thread_id: ThreadId,
    actions: impl IntoIterator<Item = &'a exoskeleton_core::tick::ActionRecord>,
) -> CodingActionFeedback {
    let mut state = CodingActionFeedback::default();
    for action in actions {
        if action.origin_exec_thread_id != Some(thread_id) {
            continue;
        }
        match (action.action_type.as_str(), action.outcome) {
            (
                "code.edit" | "code.write" | "code.apply_patch" | "fs.write",
                ActionOutcome::Success,
            ) => state.mutating_success = true,
            ("code.edit" | "code.write" | "code.apply_patch" | "fs.write", _) => {
                state.mutating_failure = true
            }
            (
                "code.read" | "code.read_symbol" | "code.grep" | "shell.exec",
                ActionOutcome::Success,
            ) => state.verification_success = true,
            _ => {}
        }
    }
    state
}
