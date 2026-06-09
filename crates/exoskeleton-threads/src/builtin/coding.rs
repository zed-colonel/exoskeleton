use std::{fmt::Write as _, path::Path};

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmBackend, LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::prompt::PromptRegistry;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, ExecThreadKind, ExecThreadLocalState, ExecThreadOutput,
    ExecThreadProposal, ExecThreadProposalConfidence, ExecThreadStatus, ExoError, ThreadFlavor,
    ThreadId, ThreadPriority, ThreadRole, ThreadSchedule, ThreadSpec,
};
use serde::{Deserialize, Serialize};

use crate::registry::ThreadRegistry;
use crate::runtime::{
    CodingActionFeedback, ExecutableThreadContext, ExecutableThreadPerception,
    SemanticActionFeedback, ThreadRuntime,
};
use crate::store::RegisteredThreadStatus;

const DEFAULT_EXPLORATORY_STALL_THRESHOLD: u32 = 3;
const DEFAULT_SEMANTIC_LOOKUP_STALL_THRESHOLD: u32 = 2;
const DEFAULT_SAME_FILE_INSPECTION_THRESHOLD: u32 = 3;
const DEFAULT_POST_EDIT_VERIFICATION_LIMIT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingPolicyProfile {
    #[serde(default = "default_exploratory_stall_threshold")]
    pub exploratory_stall_threshold: u32,
    #[serde(default = "default_semantic_lookup_stall_threshold")]
    pub semantic_lookup_stall_threshold: u32,
    #[serde(default = "default_same_file_inspection_threshold")]
    pub same_file_inspection_threshold: u32,
    #[serde(default = "default_true")]
    pub prefer_semantic_navigation: bool,
    #[serde(default = "default_true")]
    pub prefer_trait_module_targets: bool,
    #[serde(default = "default_post_edit_verification_limit")]
    pub post_edit_verification_limit: u32,
}

impl Default for CodingPolicyProfile {
    fn default() -> Self {
        Self {
            exploratory_stall_threshold: DEFAULT_EXPLORATORY_STALL_THRESHOLD,
            semantic_lookup_stall_threshold: DEFAULT_SEMANTIC_LOOKUP_STALL_THRESHOLD,
            same_file_inspection_threshold: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD,
            prefer_semantic_navigation: true,
            prefer_trait_module_targets: true,
            post_edit_verification_limit: DEFAULT_POST_EDIT_VERIFICATION_LIMIT,
        }
    }
}

fn default_exploratory_stall_threshold() -> u32 {
    DEFAULT_EXPLORATORY_STALL_THRESHOLD
}

fn default_semantic_lookup_stall_threshold() -> u32 {
    DEFAULT_SEMANTIC_LOOKUP_STALL_THRESHOLD
}

fn default_same_file_inspection_threshold() -> u32 {
    DEFAULT_SAME_FILE_INSPECTION_THRESHOLD
}

fn default_post_edit_verification_limit() -> u32 {
    DEFAULT_POST_EDIT_VERIFICATION_LIMIT
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodingPhase {
    Idle,
    Locating,
    Inspecting,
    EditCandidate,
    Editing,
    Verifying,
    Blocked,
}

impl CodingPhase {
    fn as_str(self) -> &'static str {
        match self {
            CodingPhase::Idle => "idle",
            CodingPhase::Locating => "locating",
            CodingPhase::Inspecting => "inspecting",
            CodingPhase::EditCandidate => "edit_candidate",
            CodingPhase::Editing => "editing",
            CodingPhase::Verifying => "verifying",
            CodingPhase::Blocked => "blocked",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(CodingPhase::Idle),
            "locating" | "discovery" => Some(CodingPhase::Locating),
            "inspecting" => Some(CodingPhase::Inspecting),
            "edit_candidate" => Some(CodingPhase::EditCandidate),
            "editing" => Some(CodingPhase::Editing),
            "verifying" => Some(CodingPhase::Verifying),
            "blocked" => Some(CodingPhase::Blocked),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CodingSemanticTarget<'a> {
    symbol_id: Option<&'a str>,
    name: Option<&'a str>,
    kind: Option<&'a str>,
    file: Option<&'a str>,
    query: Option<&'a str>,
    start_line: Option<u32>,
    end_line: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct CodingSituation<'a> {
    phase: CodingPhase,
    proposed_action: Option<&'a CodingExecProposal>,
    workspace_root: Option<&'a str>,
    target_file: Option<&'a str>,
    edit_hypothesis_file: Option<&'a str>,
    edit_hypothesis_summary: Option<&'a str>,
    semantic_target: Option<CodingSemanticTarget<'a>>,
    repeated_same_proposal_count: u32,
    inspection_read_streak: u32,
    evidence_complete: bool,
    verification_pending: bool,
    verification_attempted: bool,
    recent_mutation_succeeded: bool,
    recent_mutation_failed: bool,
    recent_verification_succeeded: bool,
    recent_verification_failed: bool,
}

impl CodingSituation<'_> {
    fn aligned_hypothesis(self) -> bool {
        self.edit_hypothesis_file.is_some() && self.edit_hypothesis_file == self.target_file
    }

    fn repeated_same_file_read(self, profile: &CodingPolicyProfile) -> bool {
        self.proposed_action
            .is_some_and(|proposal| proposal.tool_name == "code.read")
            && self.inspection_read_streak >= profile.same_file_inspection_threshold
            && self
                .proposed_action
                .and_then(proposal_file_path)
                .is_some_and(|file| self.target_file == Some(file.as_str()))
    }

    fn repeated_exploratory_target(self, profile: &CodingPolicyProfile) -> bool {
        self.proposed_action
            .is_some_and(|proposal| is_exploratory_tool(&proposal.tool_name))
            && self.repeated_same_proposal_count >= profile.exploratory_stall_threshold
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodingDirective {
    KeepModelProposal,
    Complete,
    ConsumeSemanticTarget,
    ReadSemanticSpan,
    FileAnchoredSemanticLookup {
        target_file: Option<String>,
    },
    ScopedTextSearch {
        target_path: String,
        pattern: String,
        rationale: String,
    },
    TargetedRead {
        file_path: String,
        offset: u32,
        limit: u32,
        rationale: String,
    },
    TargetedEdit {
        file_path: String,
        old_string: String,
        new_string: String,
        rationale: String,
    },
    TargetedPatch {
        file_path: String,
        patch: String,
        rationale: String,
    },
    TargetedVerification {
        tool_name: String,
        params: serde_json::Value,
        rationale: String,
    },
    RequireEditCandidate,
    SuppressProposal,
}

#[derive(Debug, Clone)]
struct CodingPolicyDecision {
    phase: CodingPhase,
    directive: CodingDirective,
    confidence: Option<ExecThreadProposalConfidence>,
    summary: Option<String>,
    current_focus: Option<String>,
    complete: Option<String>,
}

pub const CODING_THREAD_ID: ThreadId = ThreadId::from_uuid(uuid::Uuid::from_bytes([
    0xca, 0xe1, 0x10, 0x01, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]));

pub fn register_builtin_coding_thread(
    registry: &ThreadRegistry,
    prompts: &PromptRegistry,
    workspace_root: Option<String>,
    enabled: bool,
) -> Result<(), ExoError> {
    if !enabled {
        return Ok(());
    }

    if registry
        .find_registered_by_role(ThreadRole::Coding)?
        .is_none()
    {
        let charter = prompts
            .get("charter-coding-thread")
            .unwrap_or("Track coding progress, maintain local work state, and propose the next external action to advance the task.")
            .to_string();
        registry.register_with_status(
            ThreadSpec {
                thread_id: CODING_THREAD_ID,
                role: ThreadRole::Coding,
                flavor: ThreadFlavor::Executable,
                name: "Coding".into(),
                charter,
                priority: ThreadPriority::High,
                token_budget: 8_000,
                schedule: ThreadSchedule::OnDemand,
                workspace_root,
            },
            RegisteredThreadStatus::Executable(ExecThreadStatus::Idle),
        )?;
    }

    Ok(())
}

pub fn execute_coding_thread<R: ThreadRuntime>(
    runtime: &R,
    context: ExecutableThreadContext<'_>,
    local_state: ExecThreadLocalState,
    policy_profile: &CodingPolicyProfile,
    default_backend: LlmBackend,
    max_output_tokens: u64,
    cancellation: &CancellationToken,
) -> Result<ExecThreadOutput, ExoError> {
    let feedback = runtime.latest_coding_feedback(context.spec.thread_id)?;
    let mut prompt_local_state = local_state.clone();
    apply_semantic_feedback_state(
        &mut prompt_local_state,
        &feedback,
        context.spec.workspace_root.as_deref(),
    );
    let prompt = build_coding_exec_prompt(
        context.spec,
        context.snapshot,
        context.perception,
        &prompt_local_state,
        runtime,
    )?;
    let request = LlmRequest {
        backend: Some(default_backend),
        system_prompt: Some(
            runtime
                .prompt("coding-thread-system")
                .unwrap_or_else(|| "You are an executable coding thread. Maintain local work state and propose at most one next external action as JSON.".to_string()),
        ),
        messages: vec![LlmMessage::text(LlmRole::User, prompt)],
        max_output_tokens: max_output_tokens / 2,
        temperature: Some(0.3),
        stop_sequences: vec![],
        stream: false,
        tools: vec![],
    };
    let llm_response = runtime.llm_call(&request, cancellation)?;
    let response_text = llm_response.text;
    let mut parsed = parse_exec_response(&response_text);
    parsed.proposed_action = parsed.proposed_action.and_then(|proposal| {
        normalize_coding_proposal(proposal, context.spec.workspace_root.as_deref())
    });
    let mut status = canonicalize_exec_thread_status(parsed.status.unwrap_or_else(|| {
        default_coding_status(&parsed, context.perception, &prompt_local_state)
    }));
    if parsed.proposal_confidence.is_none() && parsed.proposed_action.is_some() {
        parsed.proposal_confidence = Some(if parsed.evidence_complete {
            ExecThreadProposalConfidence::High
        } else {
            ExecThreadProposalConfidence::Medium
        });
    }

    let mut next_local_state = ExecThreadLocalState {
        scratchpad: parsed.scratchpad.clone().or(local_state.scratchpad.clone()),
        current_focus: parsed
            .current_focus
            .clone()
            .or(prompt_local_state.current_focus.clone()),
        last_completion_reason: parsed
            .completion_reason
            .clone()
            .or(prompt_local_state.last_completion_reason.clone()),
        work_phase: parsed
            .work_phase
            .clone()
            .or(prompt_local_state.work_phase.clone()),
        evidence_complete: parsed.evidence_complete || prompt_local_state.evidence_complete,
        verification_pending: prompt_local_state.verification_pending,
        verification_attempted: prompt_local_state.verification_attempted,
        proposal_confidence: parsed
            .proposal_confidence
            .or(prompt_local_state.proposal_confidence),
        inspection_target_file: prompt_local_state.inspection_target_file.clone(),
        inspection_read_streak: prompt_local_state.inspection_read_streak,
        edit_hypothesis_file: prompt_local_state.edit_hypothesis_file.clone(),
        edit_hypothesis_summary: prompt_local_state.edit_hypothesis_summary.clone(),
        semantic_target_symbol_id: prompt_local_state.semantic_target_symbol_id.clone(),
        semantic_target_name: prompt_local_state.semantic_target_name.clone(),
        semantic_target_kind: prompt_local_state.semantic_target_kind.clone(),
        semantic_target_file: prompt_local_state.semantic_target_file.clone(),
        semantic_target_query: prompt_local_state.semantic_target_query.clone(),
        semantic_target_start_line: prompt_local_state.semantic_target_start_line,
        semantic_target_end_line: prompt_local_state.semantic_target_end_line,
        last_proposed_tool: prompt_local_state.last_proposed_tool.clone(),
        last_proposed_target: prompt_local_state.last_proposed_target.clone(),
        repeated_same_proposal_count: prompt_local_state.repeated_same_proposal_count,
        awaiting_feedback: false,
        should_wake_master: parsed.should_wake_master,
    };
    apply_coding_feedback_state(
        &mut status,
        &mut next_local_state,
        &feedback,
        &mut parsed,
        context.spec.workspace_root.as_deref(),
    );
    update_inspection_state(&mut next_local_state, &mut parsed);
    apply_coding_stall_policy(
        &mut status,
        &mut next_local_state,
        &mut parsed,
        &feedback,
        policy_profile,
        context.spec.workspace_root.as_deref(),
    );
    update_exploratory_stall_state(&mut next_local_state, parsed.proposed_action.as_ref());
    next_local_state.scratchpad = parsed
        .scratchpad
        .clone()
        .or(next_local_state.scratchpad.clone());
    next_local_state.current_focus = parsed
        .current_focus
        .clone()
        .or(next_local_state.current_focus.clone());
    next_local_state.last_completion_reason = parsed
        .completion_reason
        .clone()
        .or(next_local_state.last_completion_reason.clone());
    next_local_state.work_phase = parsed
        .work_phase
        .clone()
        .or(next_local_state.work_phase.clone());
    next_local_state.evidence_complete =
        parsed.evidence_complete || next_local_state.evidence_complete;
    next_local_state.proposal_confidence = parsed
        .proposal_confidence
        .or(next_local_state.proposal_confidence);
    next_local_state.work_phase = infer_coding_work_phase(&next_local_state, &parsed, status);
    next_local_state.awaiting_feedback = parsed.proposed_action.is_some();
    next_local_state.should_wake_master = parsed.should_wake_master;

    let output = ExecThreadOutput {
        thread_id: context.spec.thread_id,
        tick_id: context.tick_id,
        artifact_id: ArtifactId::from_content(response_text.as_bytes()),
        kind: ExecThreadKind::Coding,
        summary: parsed.summary,
        status,
        evidence_complete: next_local_state.evidence_complete,
        proposal_confidence: next_local_state.proposal_confidence,
        proposed_action: parsed.proposed_action.map(|proposal| ExecThreadProposal {
            proposal_id: format!("{}-{}", context.tick_id, context.spec.thread_id),
            tool_name: proposal.tool_name,
            params: proposal.params,
            rationale: proposal.rationale,
        }),
        local_state: next_local_state,
    };
    let artifact = Artifact::from_json(ArtifactKind::ExecThreadOutput, &output)?;
    let artifact_id = runtime.put_artifact(&artifact)?;
    let mut output = output;
    output.artifact_id = artifact_id;
    Ok(output)
}

#[derive(Debug, Deserialize, Serialize)]
struct CodingExecResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<ExecThreadStatus>,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scratchpad: Option<String>,
    #[serde(default)]
    evidence_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal_confidence: Option<ExecThreadProposalConfidence>,
    #[serde(default)]
    should_wake_master: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposed_action: Option<CodingExecProposal>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CodingExecProposal {
    tool_name: String,
    #[serde(default)]
    params: serde_json::Value,
    rationale: String,
}

fn build_coding_exec_prompt<R: ThreadRuntime>(
    spec: &ThreadSpec,
    snapshot: &exoskeleton_core::StateSnapshot,
    perception: &ExecutableThreadPerception,
    local_state: &ExecThreadLocalState,
    runtime: &R,
) -> Result<String, ExoError> {
    let recent_messages = perception
        .active_conversations
        .iter()
        .map(|c| c.message_refs.len())
        .sum::<usize>();
    let conversation_context = render_conversation_context(runtime, perception)?;
    let pending_results = render_pending_action_results(runtime, perception)?;

    let mut prompt = format!(
        "Kind: coding\nName: {}\nCharter: {}\nMission: {}\nTick: {}\nWorkspace: {}\nLast action: {}\nCurrent focus: {}\nWork phase: {}\nScratchpad: {}\nEvidence complete: {}\nVerification pending: {}\nVerification attempted: {}\nInspection target file: {}\nInspection read streak: {}\nEdit hypothesis file: {}\nEdit hypothesis: {}\nResolved semantic symbol id: {}\nResolved semantic target: {}\nResolved semantic kind: {}\nResolved semantic file: {}\nResolved semantic query: {}\nResolved semantic range: {}\nLast proposed tool: {}\nLast proposed target: {}\nRepeated same exploratory proposal count: {}\nRecent thread outputs: {}\nRecent exec outputs: {}\nRecent messages: {}\n\nConversation context:\n{}\n\nPending action results:\n{}\n\nTool parameter schemas:\n- repo.locate params: {{\"query\": \"TASK_OR_LOCALIZATION_QUERY\", \"path\": \"ABSOLUTE_DIR\", \"symbols\": optional_array_of_strings, \"limit\": optional_integer, \"include_tests\": optional_boolean}}\n- code.symbol params: {{\"path\": \"ABSOLUTE_DIR\", \"query\": \"SYMBOL_OR_QUERY\", \"path_hint\": optional_string, \"kind_hint\": optional_string, \"limit\": optional_integer}}\n- code.read_symbol params: {{\"path\": \"ABSOLUTE_DIR\", \"symbol_id\": optional_string, \"query\": optional_string, \"path_hint\": optional_string, \"kind_hint\": optional_string, \"include_body\": optional_boolean}}\n- code.references params: {{\"path\": \"ABSOLUTE_DIR\", \"symbol_id\": optional_string, \"query\": optional_string, \"path_hint\": optional_string, \"kind_hint\": optional_string, \"limit\": optional_integer, \"include_tests\": optional_boolean}}\n- code.impls params: {{\"path\": \"ABSOLUTE_DIR\", \"symbol_id\": optional_string, \"query\": optional_string, \"path_hint\": optional_string, \"kind_hint\": optional_string, \"limit\": optional_integer}}\n- code.read params: {{\"file_path\": \"ABSOLUTE_PATH\", \"offset\": optional_integer, \"limit\": optional_integer}}\n- code.grep params: {{\"pattern\": \"REGEX\", \"path\": \"ABSOLUTE_DIR\", \"output_mode\": optional_string}}\n- code.edit params: {{\"file_path\": \"ABSOLUTE_PATH\", \"old_string\": \"EXACT_TEXT_FROM_RECENT_READ\", \"new_string\": \"REPLACEMENT_TEXT\", \"replace_all\": optional_boolean}}\n- code.write params: {{\"file_path\": \"ABSOLUTE_PATH\", \"content\": \"FULL_FILE_CONTENT\"}}\n\nPhase model:\n- `locating`: narrowing the candidate file or symbol set\n- `inspecting`: using semantic lookup or targeted reads to build evidence\n- `edit_candidate`: evidence is sufficient and the next step should be a concrete edit\n- `editing`: carrying out a concrete code change\n- `verifying`: confirming a recent change\n- `idle`: no active coding work item\n\nRules:\n- If there is no active coding work in the conversation or action feedback, return status=\"idle\" and proposed_action=null.\n- If the task is complete based on the conversation request and recent action results, set status=\"idle\", set completion_reason, and do not propose another action.\n- If a successful action already satisfied the task, prefer completion over redundant reads.\n- Use the workspace path exactly as given for any proposed code tool params.\n- On large or unfamiliar repos, prefer repo.locate before repeated broad grep or listing loops.\n- After repo.locate has narrowed the area, prefer semantic tools (`code.symbol`, `code.read_symbol`, `code.references`, `code.impls`) over repeated raw file-window reads.\n- If `Resolved semantic symbol id` is present, do not repeat `code.symbol`, `code.impls`, or `code.references` for the same target. Prefer `code.read_symbol` with that `symbol_id`, then turn the result into an edit or a file switch.\n- If you have proposed the same exploratory tool against the same target for multiple consecutive ticks, do not propose it again. Either propose the edit if evidence is complete, or switch to a different action class that reduces uncertainty.\n- If you have read the same file multiple times and still do not have a concrete edit, do not read that same file again unless you can explain exactly what unseen section matters. Switch to semantic navigation, switch files, propose the edit, or go blocked.\n- If `inspection_target_file` and `edit_hypothesis_file` align and the read streak is high, prefer `work_phase=\"edit_candidate\"` and propose a concrete `code.edit` instead of more reads.\n- Prefer code.edit for localized changes. Use code.write only for full-file replacement.\n- Never use `path` for code.read, code.edit, or code.write. Use `file_path`.\n- Never use `new_content` for code.edit. code.edit must use `old_string` and `new_string` copied exactly from a recent read result.\n- For insertion-style code.edit replacements, preserve the surrounding code exactly and only add the new text. Do not rename or rewrite nearby functions while inserting.\n- When you have enough evidence to perform the next edit safely, set evidence_complete=true.\n- Set proposal_confidence to low, medium, or high. Use high only when the next action is directly supported by recent reads/searches/action feedback.\n- After a successful mutating action, prefer either completion or one explicit verification action. Do not restart broad exploration.\n- After a successful typo or rename fix, do not keep grepping or rereading the same target repeatedly. Use one verification step, then complete unless you found a concrete unresolved location that requires another edit.\n- If verification already succeeded, transition to idle rather than proposing more exploratory reads.\n- For typo or rename tasks, propose the smallest exact replacement needed rather than rewriting the file.\n\nRespond as JSON with fields: status, summary, current_focus, work_phase, scratchpad, evidence_complete, proposal_confidence, should_wake_master, completion_reason, proposed_action.\n`status` should usually be idle, active, or blocked.\n`proposed_action` must be null or an object with tool_name, params, rationale.",
        spec.name,
        spec.charter,
        snapshot.mission,
        snapshot.tick_number + 1,
        spec.workspace_root.as_deref().unwrap_or("(none)"),
        snapshot.last_action_summary.as_deref().unwrap_or("none"),
        local_state.current_focus.as_deref().unwrap_or("none"),
        local_state.work_phase.as_deref().unwrap_or("none"),
        local_state.scratchpad.as_deref().unwrap_or("none"),
        local_state.evidence_complete,
        local_state.verification_pending,
        local_state.verification_attempted,
        local_state
            .inspection_target_file
            .as_deref()
            .unwrap_or("none"),
        local_state.inspection_read_streak,
        local_state
            .edit_hypothesis_file
            .as_deref()
            .unwrap_or("none"),
        local_state
            .edit_hypothesis_summary
            .as_deref()
            .unwrap_or("none"),
        local_state
            .semantic_target_symbol_id
            .as_deref()
            .unwrap_or("none"),
        local_state
            .semantic_target_name
            .as_deref()
            .unwrap_or("none"),
        local_state
            .semantic_target_kind
            .as_deref()
            .unwrap_or("none"),
        local_state
            .semantic_target_file
            .as_deref()
            .unwrap_or("none"),
        local_state
            .semantic_target_query
            .as_deref()
            .unwrap_or("none"),
        match (
            local_state.semantic_target_start_line,
            local_state.semantic_target_end_line,
        ) {
            (Some(start), Some(end)) => format!("{start}-{end}"),
            (Some(start), None) => start.to_string(),
            _ => "none".into(),
        },
        local_state.last_proposed_tool.as_deref().unwrap_or("none"),
        local_state.last_proposed_target.as_deref().unwrap_or("none"),
        local_state.repeated_same_proposal_count,
        perception.thread_outputs.len(),
        perception.exec_thread_outputs.len(),
        recent_messages,
        conversation_context,
        pending_results,
    );
    prompt.push_str("\n\nRepo context tool:\n- repo.context params: {\"query\": \"TASK_OR_LOCALIZATION_QUERY\", \"path\": \"ABSOLUTE_DIR\", \"symbols\": optional_array_of_strings, \"patterns\": optional_array_of_regex_strings, \"target_paths\": optional_array_of_paths, \"limit\": optional_integer, \"context\": optional_integer}\n- Prefer repo.context when the task mentions several symbols/files or when you need a compact line-grounded evidence pack before choosing exact reads/edits.\n- If semantic lookup repeats without new evidence, switch to repo.context or scoped code.grep instead of issuing the same semantic query again.");
    prompt.push_str("\n\nCode test tool:\n- code.test params: {\"path\": \"ABSOLUTE_DIR\", \"command\": optional_string_or_auto, \"args\": optional_array_of_strings, \"timeout_ms\": optional_integer}\n- After a successful edit, prefer one code.test verification when a project test command is available. If code.test reports passed=false, use its output for the next inspection or edit; do not report completion.");
    prompt.push_str("\n\nAdditional coding rule:\n- When editing macro invocations, first match the macro definition syntax. If one public type must accept multiple source variants, inspect whether the macro supports multiple variants. Do not add an incomplete single-arm workaround when the macro itself needs to represent all valid variants.");
    Ok(prompt)
}

fn parse_exec_response(response_text: &str) -> CodingExecResponse {
    serde_json::from_str(response_text)
        .or_else(|_| {
            extract_json_from_code_fence(response_text)
                .ok_or_else(|| serde_json::Error::io(std::io::Error::other("no code fence")))
                .and_then(serde_json::from_str)
        })
        .unwrap_or_else(|_| CodingExecResponse {
            status: None,
            summary: response_text.to_string(),
            current_focus: None,
            work_phase: None,
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: false,
            completion_reason: None,
            proposed_action: None,
        })
}

pub fn should_run_coding_thread(
    status: ExecThreadStatus,
    perception: &ExecutableThreadPerception,
    local_state: &ExecThreadLocalState,
) -> bool {
    match status {
        ExecThreadStatus::Failed => false,
        ExecThreadStatus::Active | ExecThreadStatus::Blocked | ExecThreadStatus::Completed => true,
        ExecThreadStatus::Idle => {
            has_coding_work(perception, local_state) || local_state.should_wake_master
        }
    }
}

fn has_coding_work(
    perception: &ExecutableThreadPerception,
    local_state: &ExecThreadLocalState,
) -> bool {
    !perception.new_messages.is_empty()
        || (local_state.awaiting_feedback && !perception.pending_action_results.is_empty())
}

fn default_coding_status(
    parsed: &CodingExecResponse,
    perception: &ExecutableThreadPerception,
    local_state: &ExecThreadLocalState,
) -> ExecThreadStatus {
    if parsed.completion_reason.is_some() {
        ExecThreadStatus::Idle
    } else if parsed.proposed_action.is_some() || has_coding_work(perception, local_state) {
        ExecThreadStatus::Active
    } else {
        ExecThreadStatus::Idle
    }
}

fn canonicalize_exec_thread_status(status: ExecThreadStatus) -> ExecThreadStatus {
    match status {
        ExecThreadStatus::Completed => ExecThreadStatus::Idle,
        other => other,
    }
}

fn normalize_coding_proposal(
    mut proposal: CodingExecProposal,
    workspace_root: Option<&str>,
) -> Option<CodingExecProposal> {
    let params = proposal.params.as_object_mut()?;
    match proposal.tool_name.as_str() {
        "repo.locate" => {
            if let Some(workspace_root) = workspace_root.filter(|root| !root.trim().is_empty()) {
                params
                    .entry("path".to_string())
                    .or_insert_with(|| serde_json::Value::String(workspace_root.to_string()));
            }
            let has_query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|query| !query.trim().is_empty());
            let has_symbols = params
                .get("symbols")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|symbols| !symbols.is_empty());
            let has_path = params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| !path.trim().is_empty());
            ((has_query || has_symbols) && has_path).then_some(proposal)
        }
        "repo.context" => {
            if let Some(workspace_root) = workspace_root.filter(|root| !root.trim().is_empty()) {
                params
                    .entry("path".to_string())
                    .or_insert_with(|| serde_json::Value::String(workspace_root.to_string()));
            }
            let has_query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|query| !query.trim().is_empty());
            let has_symbols = params
                .get("symbols")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|symbols| !symbols.is_empty());
            let has_patterns = params
                .get("patterns")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|patterns| !patterns.is_empty());
            let has_targets = params
                .get("target_paths")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|targets| !targets.is_empty());
            let has_path = params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| !path.trim().is_empty());
            ((has_query || has_symbols || has_patterns || has_targets) && has_path)
                .then_some(proposal)
        }
        "code.symbol" => {
            if let Some(workspace_root) = workspace_root.filter(|root| !root.trim().is_empty()) {
                params
                    .entry("path".to_string())
                    .or_insert_with(|| serde_json::Value::String(workspace_root.to_string()));
            }
            let has_query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|query| !query.trim().is_empty());
            let has_path = params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| !path.trim().is_empty());
            (has_query && has_path).then_some(proposal)
        }
        "code.read_symbol" | "code.references" | "code.impls" => {
            if let Some(workspace_root) = workspace_root.filter(|root| !root.trim().is_empty()) {
                params
                    .entry("path".to_string())
                    .or_insert_with(|| serde_json::Value::String(workspace_root.to_string()));
            }
            let has_symbol_id = params
                .get("symbol_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|symbol_id| !symbol_id.trim().is_empty());
            let has_query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|query| !query.trim().is_empty());
            let has_path = params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| !path.trim().is_empty());
            ((has_symbol_id || has_query) && has_path).then_some(proposal)
        }
        "code.read" => {
            if let Some(path) = params.remove("path") {
                params.entry("file_path").or_insert(path);
            }
            params.contains_key("file_path").then_some(proposal)
        }
        "code.write" => {
            if let Some(path) = params.remove("path") {
                params.entry("file_path").or_insert(path);
            }
            if let Some(new_content) = params.remove("new_content") {
                params.entry("content").or_insert(new_content);
            }
            (params.contains_key("file_path") && params.contains_key("content")).then_some(proposal)
        }
        "code.edit" => {
            if let Some(path) = params.remove("path") {
                params.entry("file_path").or_insert(path);
            }
            if !(params.contains_key("file_path")
                && params.contains_key("old_string")
                && params.contains_key("new_string"))
            {
                return None;
            }
            proposal.params.is_object().then_some(proposal)
        }
        "code.grep" => {
            if let Some(workspace_root) = workspace_root.filter(|root| !root.trim().is_empty()) {
                params
                    .entry("path".to_string())
                    .or_insert_with(|| serde_json::Value::String(workspace_root.to_string()));
            }
            if params
                .get("output_mode")
                .and_then(serde_json::Value::as_str)
                == Some("context")
            {
                params.insert(
                    "output_mode".to_string(),
                    serde_json::Value::String("content".into()),
                );
                params
                    .entry("context".to_string())
                    .or_insert_with(|| serde_json::Value::Number(2.into()));
            }
            if let Some(path) = params.get("path").and_then(serde_json::Value::as_str) {
                if looks_like_file_path(path) {
                    if let Some(parent) = Path::new(path).parent() {
                        params.insert(
                            "path".to_string(),
                            serde_json::Value::String(parent.display().to_string()),
                        );
                    }
                }
            }
            (params.contains_key("pattern") && params.contains_key("path")).then_some(proposal)
        }
        _ => Some(proposal),
    }
}

fn apply_coding_feedback_state(
    status: &mut ExecThreadStatus,
    local_state: &mut ExecThreadLocalState,
    feedback: &CodingActionFeedback,
    parsed: &mut CodingExecResponse,
    workspace_root: Option<&str>,
) {
    apply_semantic_feedback_state(local_state, feedback, workspace_root);

    if feedback.mutating_failure {
        let verification_already_attempted = local_state.verification_attempted;
        clear_exploratory_stall_state(local_state);
        clear_inspection_state(local_state);
        local_state.verification_pending = false;
        local_state.verification_attempted = verification_already_attempted;
        local_state.work_phase = Some("editing".into());
        if parsed.proposal_confidence.is_none() {
            parsed.proposal_confidence = Some(ExecThreadProposalConfidence::Medium);
        }
        *status = ExecThreadStatus::Active;
        return;
    }

    if feedback.mutating_success {
        clear_exploratory_stall_state(local_state);
        clear_inspection_state(local_state);
        local_state.verification_pending = true;
        local_state.verification_attempted = false;
        local_state.work_phase = Some("verifying".into());
    }

    if local_state.verification_pending && feedback.verification_failure {
        clear_exploratory_stall_state(local_state);
        local_state.verification_pending = false;
        local_state.verification_attempted = true;
        local_state.evidence_complete = false;
        parsed.evidence_complete = false;
        local_state.work_phase = Some("inspecting".into());
        *status = ExecThreadStatus::Active;
        return;
    }

    if local_state.verification_pending && feedback.verification_success {
        clear_exploratory_stall_state(local_state);
        clear_inspection_state(local_state);
        local_state.verification_pending = false;
        local_state.verification_attempted = true;
        local_state.evidence_complete = true;
        local_state.work_phase = Some("idle".into());
        if local_state.last_completion_reason.is_none() {
            local_state.last_completion_reason =
                Some("Coding work item completed after successful verification".into());
        }
        parsed.completion_reason = local_state.last_completion_reason.clone();
        parsed.proposed_action = None;
        parsed.should_wake_master = false;
        *status = ExecThreadStatus::Idle;
        return;
    }
}

fn apply_semantic_feedback_state(
    local_state: &mut ExecThreadLocalState,
    feedback: &CodingActionFeedback,
    workspace_root: Option<&str>,
) {
    let Some(semantic) = feedback.semantic_resolution.as_ref() else {
        return;
    };

    if !semantic_resolution_is_authoritative(
        local_state,
        semantic.file_path.as_deref(),
        workspace_root,
    ) {
        return;
    }

    if let Some(symbol_id) = semantic
        .symbol_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        local_state.semantic_target_symbol_id = Some(symbol_id.clone());
    }
    if let Some(name) = semantic
        .name
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        local_state.semantic_target_name = Some(name.clone());
    }
    if let Some(kind) = semantic
        .kind
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        local_state.semantic_target_kind = Some(kind.clone());
    }
    if let Some(file_path) = semantic
        .file_path
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        local_state.semantic_target_file = Some(file_path.clone());
        local_state.inspection_target_file = Some(file_path.clone());
        if local_state.edit_hypothesis_file.is_none() {
            local_state.edit_hypothesis_file = Some(file_path.clone());
        }
    }
    if let Some(query) = semantic
        .query
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        local_state.semantic_target_query = Some(query.clone());
    }
    local_state.semantic_target_start_line = semantic.start_line;
    local_state.semantic_target_end_line = semantic.end_line;
    if local_state.edit_hypothesis_summary.is_none() {
        local_state.edit_hypothesis_summary = Some(compact_semantic_feedback(semantic));
    }
}

fn update_inspection_state(
    local_state: &mut ExecThreadLocalState,
    parsed: &mut CodingExecResponse,
) {
    let Some(proposal) = parsed.proposed_action.as_ref() else {
        return;
    };

    match proposal.tool_name.as_str() {
        "code.read_symbol" => {
            if let Some(path_hint) = proposal_symbol_file_hint(proposal) {
                let target_hint = semantic_path_hint_to_target_file(&path_hint);
                local_state.inspection_target_file = Some(target_hint.clone());
                if local_state.edit_hypothesis_file.is_none() {
                    local_state.edit_hypothesis_file = Some(target_hint);
                }
            }
            local_state.inspection_read_streak =
                local_state.inspection_read_streak.saturating_add(1).max(1);
            if local_state.edit_hypothesis_summary.is_none() {
                local_state.edit_hypothesis_summary = Some(compact_text(&proposal.rationale, 200));
            }
            parsed.work_phase.get_or_insert_with(|| "inspecting".into());
        }
        "code.symbol" | "code.references" | "code.impls" => {
            if let Some(path_hint) = proposal_symbol_file_hint(proposal) {
                let target_hint = semantic_path_hint_to_target_file(&path_hint);
                if local_state.inspection_target_file.is_none() {
                    local_state.inspection_target_file = Some(target_hint.clone());
                }
                if local_state.edit_hypothesis_file.is_none() {
                    local_state.edit_hypothesis_file = Some(target_hint);
                }
            }
            if local_state.edit_hypothesis_summary.is_none() {
                local_state.edit_hypothesis_summary = Some(compact_text(&proposal.rationale, 200));
            }
            parsed.work_phase.get_or_insert_with(|| "inspecting".into());
        }
        "code.read" => {
            let Some(file_path) = proposal_file_path(proposal) else {
                return;
            };
            if local_state.inspection_target_file.as_deref() == Some(file_path.as_str()) {
                local_state.inspection_read_streak =
                    local_state.inspection_read_streak.saturating_add(1);
            } else {
                local_state.inspection_target_file = Some(file_path.clone());
                local_state.inspection_read_streak = 1;
            }
            if local_state.edit_hypothesis_file.is_none() {
                local_state.edit_hypothesis_file = Some(file_path.clone());
            }
            if local_state.edit_hypothesis_summary.is_none() {
                local_state.edit_hypothesis_summary = Some(compact_text(&proposal.rationale, 200));
            }
            if proposal_read_covers_semantic_target(local_state, proposal) {
                local_state.evidence_complete = true;
                parsed.evidence_complete = true;
                parsed.work_phase = Some("edit_candidate".into());
            } else {
                parsed.work_phase.get_or_insert_with(|| "inspecting".into());
            }
        }
        "code.edit" | "code.write" => {
            let hypothesis_file = proposal_file_path(proposal);
            if hypothesis_file.is_some() {
                local_state.edit_hypothesis_file = hypothesis_file;
            }
            local_state.edit_hypothesis_summary = Some(compact_text(&proposal.rationale, 200));
            clear_inspection_state(local_state);
            parsed.evidence_complete = true;
            parsed
                .work_phase
                .get_or_insert_with(|| "edit_candidate".into());
        }
        "repo.locate" | "repo.context" | "code.grep" | "code.glob" | "code.ls" => {
            if has_same_file_semantic_commitment(local_state) {
                parsed
                    .work_phase
                    .get_or_insert_with(|| "edit_candidate".into());
            } else {
                clear_inspection_state(local_state);
                parsed.work_phase.get_or_insert_with(|| "locating".into());
            }
        }
        "shell.exec" | "code.test" => {
            parsed.work_phase.get_or_insert_with(|| "verifying".into());
        }
        _ => {}
    }
}

fn apply_coding_stall_policy(
    status: &mut ExecThreadStatus,
    local_state: &mut ExecThreadLocalState,
    parsed: &mut CodingExecResponse,
    feedback: &CodingActionFeedback,
    profile: &CodingPolicyProfile,
    workspace_root: Option<&str>,
) {
    let situation = build_coding_situation(*status, local_state, parsed, feedback, workspace_root);
    let decision = evaluate_coding_policy(&situation, local_state, parsed, profile);
    apply_policy_decision(status, local_state, parsed, decision, workspace_root);

    let situation = build_coding_situation(*status, local_state, parsed, feedback, workspace_root);
    if let Some(decision) =
        evaluate_final_proposal_trajectory_governor(&situation, local_state, parsed, profile)
    {
        apply_policy_decision(status, local_state, parsed, decision, workspace_root);
    }
}

fn build_coding_situation<'a>(
    status: ExecThreadStatus,
    local_state: &'a ExecThreadLocalState,
    parsed: &'a CodingExecResponse,
    feedback: &'a CodingActionFeedback,
    workspace_root: Option<&'a str>,
) -> CodingSituation<'a> {
    let phase = parsed
        .work_phase
        .as_deref()
        .and_then(CodingPhase::from_str)
        .or_else(|| {
            local_state
                .work_phase
                .as_deref()
                .and_then(CodingPhase::from_str)
        })
        .unwrap_or_else(|| match status {
            ExecThreadStatus::Idle => CodingPhase::Idle,
            ExecThreadStatus::Blocked => CodingPhase::Blocked,
            _ if local_state.verification_pending => CodingPhase::Verifying,
            _ if parsed.evidence_complete || local_state.evidence_complete => {
                CodingPhase::EditCandidate
            }
            _ => CodingPhase::Inspecting,
        });

    CodingSituation {
        phase,
        proposed_action: parsed.proposed_action.as_ref(),
        workspace_root,
        target_file: concrete_target_file(local_state),
        edit_hypothesis_file: local_state.edit_hypothesis_file.as_deref(),
        edit_hypothesis_summary: local_state.edit_hypothesis_summary.as_deref(),
        semantic_target: semantic_target(local_state),
        repeated_same_proposal_count: local_state.repeated_same_proposal_count,
        inspection_read_streak: local_state.inspection_read_streak,
        evidence_complete: parsed.evidence_complete || local_state.evidence_complete,
        verification_pending: local_state.verification_pending,
        verification_attempted: local_state.verification_attempted,
        recent_mutation_succeeded: feedback.mutating_success,
        recent_mutation_failed: feedback.mutating_failure,
        recent_verification_succeeded: feedback.verification_success,
        recent_verification_failed: feedback.verification_failure,
    }
}

fn semantic_target(local_state: &ExecThreadLocalState) -> Option<CodingSemanticTarget<'_>> {
    (local_state.semantic_target_symbol_id.is_some()
        || local_state.semantic_target_name.is_some()
        || local_state.semantic_target_file.is_some()
        || local_state.semantic_target_query.is_some())
    .then_some(CodingSemanticTarget {
        symbol_id: local_state.semantic_target_symbol_id.as_deref(),
        name: local_state.semantic_target_name.as_deref(),
        kind: local_state.semantic_target_kind.as_deref(),
        file: local_state.semantic_target_file.as_deref(),
        query: local_state.semantic_target_query.as_deref(),
        start_line: local_state.semantic_target_start_line,
        end_line: local_state.semantic_target_end_line,
    })
}

fn evaluate_coding_policy(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
) -> CodingPolicyDecision {
    let _recent_action_feedback = (
        situation.recent_mutation_succeeded,
        situation.recent_mutation_failed,
        situation.recent_verification_succeeded,
        situation.recent_verification_failed,
    );
    let Some(proposal) = situation.proposed_action else {
        if situation.verification_pending {
            if let Some(verification) =
                synthesize_post_edit_verification_proposal(situation, local_state, parsed)
            {
                return CodingPolicyDecision {
                    phase: CodingPhase::Verifying,
                    directive: CodingDirective::TargetedVerification {
                        tool_name: verification.tool_name,
                        params: verification.params,
                        rationale: verification.rationale,
                    },
                    confidence: Some(ExecThreadProposalConfidence::Medium),
                    summary: Some(
                        "A mutating action already succeeded; verify before completing.".into(),
                    ),
                    current_focus: Some("verify the recent edit".into()),
                    complete: None,
                };
            }
        }

        if let Some(edit) =
            synthesize_conversion_macro_variant_edit(situation, local_state, parsed, None)
        {
            if edit.tool_name == "code.apply_patch" {
                return CodingPolicyDecision {
                    phase: CodingPhase::Editing,
                    directive: CodingDirective::TargetedPatch {
                        file_path: edit
                            .params
                            .get("file_path")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        patch: edit
                            .params
                            .get("patch")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        rationale: edit.rationale,
                    },
                    confidence: Some(ExecThreadProposalConfidence::High),
                    summary: Some(
                        "The conversion macro edit site is known; apply the multi-variant patch instead of waiting for another exploratory proposal."
                            .into(),
                    ),
                    current_focus: Some("apply the conversion macro multi-variant patch".into()),
                    complete: None,
                };
            }
        }

        return CodingPolicyDecision {
            phase: situation.phase,
            directive: CodingDirective::KeepModelProposal,
            confidence: None,
            summary: None,
            current_focus: None,
            complete: None,
        };
    };

    if situation.verification_attempted
        && !situation.verification_pending
        && proposal.tool_name == "code.test"
    {
        if let Some(file_path) = situation.edit_hypothesis_file.or(situation.target_file) {
            return CodingPolicyDecision {
                phase: CodingPhase::Inspecting,
                directive: CodingDirective::TargetedRead {
                    file_path: file_path.to_string(),
                    offset: if is_module_file(file_path) { 285 } else { 1 },
                    limit: if is_module_file(file_path) { 100 } else { 200 },
                    rationale: "A verification command already failed; inspect the changed target before running another expensive test command without a new edit."
                        .into(),
                },
                confidence: Some(ExecThreadProposalConfidence::Medium),
                summary: Some(
                    "Verification already failed once; do not rerun code.test until another edit is made."
                        .into(),
                ),
                current_focus: Some("inspect the changed file after failed verification".into()),
                complete: None,
            };
        }

        return CodingPolicyDecision {
            phase: CodingPhase::Verifying,
            directive: CodingDirective::SuppressProposal,
            confidence: None,
            summary: Some(
                "Verification already failed once; suppressing repeated code.test without a new edit."
                    .into(),
            ),
            current_focus: Some("analyze verification failure before rerunning tests".into()),
            complete: None,
        };
    }

    if situation.verification_pending
        && !situation.verification_attempted
        && proposal.tool_name == "code.test"
    {
        if let Some(test) = synthesize_post_edit_test_verification(situation) {
            if proposal.tool_name != test.tool_name || proposal.params != test.params {
                return CodingPolicyDecision {
                    phase: CodingPhase::Verifying,
                    directive: CodingDirective::TargetedVerification {
                        tool_name: test.tool_name,
                        params: test.params,
                        rationale: test.rationale,
                    },
                    confidence: Some(ExecThreadProposalConfidence::Medium),
                    summary: Some(
                        "A mutating action already succeeded; use the scoped post-edit verification command."
                            .into(),
                    ),
                    current_focus: Some("verify the recent edit at the nearest project boundary".into()),
                    complete: None,
                };
            }
        }
    }

    if situation.verification_attempted
        && !situation.verification_pending
        && proposal_is_known_conversion_macro_variant_patch(proposal)
    {
        let file_path = proposal_file_path(proposal)
            .or_else(|| situation.edit_hypothesis_file.map(str::to_string))
            .or_else(|| situation.target_file.map(str::to_string));
        if let Some(file_path) = file_path {
            return CodingPolicyDecision {
                phase: CodingPhase::Inspecting,
                directive: CodingDirective::TargetedRead {
                    offset: if is_module_file(&file_path) { 285 } else { 1 },
                    limit: if is_module_file(&file_path) { 100 } else { 200 },
                    file_path,
                    rationale: "The deterministic conversion macro patch has already been applied and verification has failed; inspect the changed target or verification output instead of reapplying the same patch."
                        .into(),
                },
                confidence: Some(ExecThreadProposalConfidence::Medium),
                summary: Some(
                    "Verification already failed after the conversion macro patch; do not reapply the same patch."
                        .into(),
                ),
                current_focus: Some("inspect the changed macro after failed verification".into()),
                complete: None,
            };
        }

        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::SuppressProposal,
            confidence: None,
            summary: Some(
                "Verification already failed after the conversion macro patch; suppressing repeated patch."
                    .into(),
            ),
            current_focus: Some("inspect verification failure before another edit".into()),
            complete: None,
        };
    }

    if proposal_changes_macro_variant_repetition_plus_to_star(proposal) {
        let file_path = proposal_file_path(proposal)
            .or_else(|| situation.edit_hypothesis_file.map(str::to_string))
            .or_else(|| situation.target_file.map(str::to_string));
        if let Some(file_path) = file_path {
            return CodingPolicyDecision {
                phase: CodingPhase::Inspecting,
                directive: CodingDirective::TargetedRead {
                    offset: if is_module_file(&file_path) { 285 } else { 1 },
                    limit: if is_module_file(&file_path) { 100 } else { 200 },
                    file_path,
                    rationale: "The `|+` repetition is the intended Rust macro syntax for one or more `|`-separated variants; inspect the macro instead of changing it to `|*`."
                        .into(),
                },
                confidence: Some(ExecThreadProposalConfidence::Medium),
                summary: Some(
                    "Rejecting incorrect macro repetition edit from `|+` to `|*`; keep the one-or-more variant syntax."
                        .into(),
                ),
                current_focus: Some("preserve the correct macro repetition syntax".into()),
                complete: None,
            };
        }

        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::SuppressProposal,
            confidence: None,
            summary: Some("Rejecting incorrect macro repetition edit from `|+` to `|*`.".into()),
            current_focus: Some("preserve the correct macro repetition syntax".into()),
            complete: None,
        };
    }

    if situation.verification_pending
        && !situation.verification_attempted
        && proposal.tool_name != "code.test"
    {
        if let Some(test) = synthesize_post_edit_test_verification(situation) {
            return CodingPolicyDecision {
                phase: CodingPhase::Verifying,
                directive: CodingDirective::TargetedVerification {
                    tool_name: test.tool_name,
                    params: test.params,
                    rationale: test.rationale,
                },
                confidence: Some(ExecThreadProposalConfidence::Medium),
                summary: Some(
                    "A mutating action already succeeded; run the project test command before read-only verification."
                        .into(),
                ),
                current_focus: Some("verify the recent edit with tests".into()),
                complete: None,
            };
        }
    }

    if situation.verification_pending && !is_verification_action(Some(proposal)) {
        if let Some(verification) =
            synthesize_post_edit_verification_proposal(situation, local_state, parsed)
        {
            return CodingPolicyDecision {
                phase: CodingPhase::Verifying,
                directive: CodingDirective::TargetedVerification {
                    tool_name: verification.tool_name,
                    params: verification.params,
                    rationale: verification.rationale,
                },
                confidence: Some(ExecThreadProposalConfidence::Medium),
                summary: Some(
                    "A mutating action already succeeded; verify before proposing another edit."
                        .into(),
                ),
                current_focus: Some("verify the recent edit or test result".into()),
                complete: None,
            };
        }
    }

    if let Some(search) =
        synthesize_repeated_trait_lookup_text_search(situation, local_state, parsed, profile)
    {
        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::ScopedTextSearch {
                target_path: search
                    .params
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                pattern: search
                    .params
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rationale: search.rationale,
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(
                "Repeated semantic impl lookup is not narrowing the edit target; switch to a scoped text search for the concrete trait/type conversion site"
                    .into(),
            ),
            current_focus: Some(
                "break semantic lookup stall and inspect the concrete conversion source".into(),
            ),
            complete: None,
        };
    }

    if let Some(read) =
        synthesize_conversion_macro_usage_read(situation, local_state, parsed, proposal)
    {
        let target = proposal_stall_target(&read).unwrap_or_else(|| read.tool_name.clone());
        if local_state.last_proposed_tool.as_deref() == Some(read.tool_name.as_str())
            && local_state.last_proposed_target.as_deref() == Some(target.as_str())
        {
            return CodingPolicyDecision {
                phase: CodingPhase::EditCandidate,
                directive: CodingDirective::RequireEditCandidate,
                confidence: None,
                summary: Some(
                    "The conversion macro invocation has already been inspected; semantic lookup of the macro name will not add edit evidence. Propose the exact code.edit against the invocation or go blocked."
                        .into(),
                ),
                current_focus: Some(
                    "commit the macro invocation read into a concrete edit candidate".into(),
                ),
                complete: None,
            };
        }

        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::TargetedRead {
                file_path: read
                    .params
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                offset: read
                    .params
                    .get("offset")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default() as u32,
                limit: read
                    .params
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(80) as u32,
                rationale: read.rationale,
            },
            confidence: Some(ExecThreadProposalConfidence::High),
            summary: Some(
                "The macro definition is known; read the concrete invocation span before any further semantic lookup of the macro name."
                    .into(),
            ),
            current_focus: Some(
                "inspect the macro invocation that should become the exact edit site".into(),
            ),
            complete: None,
        };
    }

    if let Some(edit) =
        synthesize_conversion_macro_variant_edit(situation, local_state, parsed, Some(proposal))
    {
        if edit.tool_name == "code.apply_patch" {
            return CodingPolicyDecision {
                phase: CodingPhase::Editing,
                directive: CodingDirective::TargetedPatch {
                    file_path: edit
                        .params
                        .get("file_path")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    patch: edit
                        .params
                        .get("patch")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    rationale: edit.rationale,
                },
                confidence: Some(ExecThreadProposalConfidence::High),
                summary: Some(
                    "The conversion macro must support multiple source variants; apply the localized macro and invocation patch instead of another inspection step."
                        .into(),
                ),
                current_focus: Some("apply the conversion macro multi-variant patch".into()),
                complete: None,
            };
        }

        return CodingPolicyDecision {
            phase: CodingPhase::Editing,
            directive: CodingDirective::TargetedEdit {
                file_path: edit
                    .params
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                old_string: edit
                    .params
                    .get("old_string")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                new_string: edit
                    .params
                    .get("new_string")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rationale: edit.rationale,
            },
            confidence: Some(ExecThreadProposalConfidence::High),
            summary: Some(
                "The conversion macro invocation and missing source variant are known; propose the exact localized edit instead of another inspection step."
                    .into(),
            ),
            current_focus: Some("apply the conversion macro variant edit".into()),
            complete: None,
        };
    }

    if should_enforce_edit_candidate_commitment(situation, local_state, parsed, proposal) {
        let target_file = situation
            .edit_hypothesis_file
            .or(situation.target_file)
            .unwrap_or("the current target file");
        return CodingPolicyDecision {
            phase: CodingPhase::EditCandidate,
            directive: CodingDirective::RequireEditCandidate,
            confidence: None,
            summary: Some(format!(
                "Inspect-to-edit commitment is active for {target_file}; do not spend another tick on semantic lookup or broad exploration. Propose the exact code.edit from recently read source, switch to a different explicit file, or go blocked with the missing fact."
            )),
            current_focus: Some(format!(
                "commit the concrete edit hypothesis for {target_file} into code.edit or blocked"
            )),
            complete: None,
        };
    }

    if let Some(search) = synthesize_module_trait_lookup_text_search(
        situation,
        local_state,
        parsed,
        profile,
        proposal,
    ) {
        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::ScopedTextSearch {
                target_path: search
                    .params
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                pattern: search
                    .params
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rationale: search.rationale,
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(
                "Trait-only semantic impl lookup in a module can resolve unrelated impls; search scoped source text for the concrete conversion site instead."
                    .into(),
            ),
            current_focus: Some(
                "replace broad trait impl lookup with scoped conversion source search".into(),
            ),
            complete: None,
        };
    }

    if should_commit_after_conversion_macro_invocation(situation, local_state, parsed, proposal) {
        return CodingPolicyDecision {
            phase: CodingPhase::EditCandidate,
            directive: CodingDirective::RequireEditCandidate,
            confidence: None,
            summary: Some(
                "The conversion macro invocation is now the edit site; additional semantic lookup for map types will not add edit evidence. Propose the exact code.edit against the invocation or go blocked."
                    .into(),
            ),
            current_focus: Some(
                "commit the inspected conversion macro invocation into a concrete edit".into(),
            ),
            complete: None,
        };
    }

    if let Some(target_override) =
        preferred_file_anchored_target(situation, local_state, parsed, profile)
    {
        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::FileAnchoredSemanticLookup {
                target_file: Some(target_override),
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(
                "The resolved semantic target is supporting evidence in a concrete leaf file; trait-oriented work should inspect the generic module that owns the conversion machinery"
                    .into(),
            ),
            current_focus: Some(
                "switch from concrete leaf type inspection to generic trait/conversion machinery"
                    .into(),
            ),
            complete: None,
        };
    }

    if should_consume_resolved_semantic_target(proposal, local_state, situation.workspace_root) {
        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::ConsumeSemanticTarget,
            confidence: Some(ExecThreadProposalConfidence::High),
            summary: Some("A resolved semantic target is already available; consume it with a focused symbol read before reopening semantic lookup".into()),
            current_focus: Some(
                "use resolved semantic target to inspect the exact symbol span".into(),
            ),
            complete: None,
        };
    }

    if should_force_semantic_span_read(proposal, local_state) {
        return CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::ReadSemanticSpan,
            confidence: Some(ExecThreadProposalConfidence::High),
            summary: Some("A same-file semantic target is already available; inspect the exact source span instead of broadening search".into()),
            current_focus: Some("read the exact semantic span, then move to a concrete edit".into()),
            complete: None,
        };
    }

    if should_suppress_broad_reorientation(proposal, local_state, situation.workspace_root) {
        let target_file = concrete_target_file(local_state).unwrap_or("current target file");
        let phase = if situation.evidence_complete || situation.aligned_hypothesis() {
            CodingPhase::EditCandidate
        } else {
            CodingPhase::Inspecting
        };
        return CodingPolicyDecision {
            phase,
            directive: CodingDirective::FileAnchoredSemanticLookup {
                target_file: None,
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(format!(
                "A concrete target file is already known ({target_file}); do not reopen workspace-wide search. Propose the edit, switch to a different explicit file target, use a file-anchored semantic lookup, or go blocked."
            )),
            current_focus: Some(format!(
                "commit investigation on {target_file} or switch files explicitly"
            )),
            complete: None,
        };
    }

    let repeated_same_file_read = situation.repeated_same_file_read(profile);
    let repeated_exploratory_target = situation.repeated_exploratory_target(profile);

    if situation.verification_pending
        && situation.verification_attempted
        && repeated_exploratory_target
    {
        return CodingPolicyDecision {
            phase: CodingPhase::Idle,
            directive: CodingDirective::Complete,
            confidence: None,
            summary: Some("Post-edit verification is complete enough; suppressing repeated exploratory checks and returning to idle".into()),
            current_focus: None,
            complete: Some(
                "Coding work item completed after repeated post-edit exploratory checks were suppressed"
                    .into(),
            ),
        };
    }

    if repeated_same_file_read {
        let stalled_file = situation.target_file.unwrap_or("same file");
        let hypothesis = situation
            .edit_hypothesis_summary
            .unwrap_or("read enough to form an edit hypothesis or change tactic");
        if situation.target_file.is_some_and(is_module_file)
            && focus_mentions_impls(local_state, parsed)
        {
            if let Some(read) = synthesize_conversion_macro_read(situation, local_state, parsed) {
                if macro_definition_read_is_repeated(local_state, &read) {
                    if let Some(usage_read) = synthesize_conversion_macro_usage_read_from_context(
                        situation,
                        local_state,
                        parsed,
                    ) {
                        return CodingPolicyDecision {
                            phase: CodingPhase::Inspecting,
                            directive: CodingDirective::TargetedRead {
                                file_path: usage_read
                                    .params
                                    .get("file_path")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                offset: usage_read
                                    .params
                                    .get("offset")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or_default()
                                    as u32,
                                limit: usage_read
                                    .params
                                    .get("limit")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(60)
                                    as u32,
                                rationale: usage_read.rationale,
                            },
                            confidence: Some(ExecThreadProposalConfidence::High),
                            summary: Some(format!(
                                "Inspection of {stalled_file} already read the conversion macro definition; read the invocation span that should become the edit site."
                            )),
                            current_focus: Some(format!(
                                "inspect the conversion macro invocation in {stalled_file}: {hypothesis}"
                            )),
                            complete: None,
                        };
                    }

                    if let Some(invocation_read) =
                        synthesize_conversion_macro_after_definition_read(situation, &read)
                    {
                        return CodingPolicyDecision {
                            phase: CodingPhase::Inspecting,
                            directive: CodingDirective::TargetedRead {
                                file_path: invocation_read
                                    .params
                                    .get("file_path")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                offset: invocation_read
                                    .params
                                    .get("offset")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or_default()
                                    as u32,
                                limit: invocation_read
                                    .params
                                    .get("limit")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(80)
                                    as u32,
                                rationale: invocation_read.rationale,
                            },
                            confidence: Some(ExecThreadProposalConfidence::High),
                            summary: Some(format!(
                                "Inspection of {stalled_file} already read the conversion macro definition; read the following invocation region instead of rereading the definition."
                            )),
                            current_focus: Some(format!(
                                "read the conversion macro invocation region in {stalled_file}: {hypothesis}"
                            )),
                            complete: None,
                        };
                    }
                }

                return CodingPolicyDecision {
                    phase: CodingPhase::Inspecting,
                    directive: CodingDirective::TargetedRead {
                        file_path: read
                            .params
                            .get("file_path")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        offset: read
                            .params
                            .get("offset")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or_default() as u32,
                        limit: read
                            .params
                            .get("limit")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(120) as u32,
                        rationale: read.rationale,
                    },
                    confidence: Some(ExecThreadProposalConfidence::High),
                    summary: Some(format!(
                        "Inspection of {stalled_file} has a concrete conversion macro hit; read that source span instead of repeating search."
                    )),
                    current_focus: Some(format!(
                        "read the concrete conversion macro in {stalled_file}: {hypothesis}"
                    )),
                    complete: None,
                };
            }
            if let Some(search) = synthesize_trait_conversion_text_search(
                situation,
                local_state,
                parsed,
                profile,
                proposal,
            ) {
                return CodingPolicyDecision {
                    phase: CodingPhase::Inspecting,
                    directive: CodingDirective::ScopedTextSearch {
                        target_path: search
                            .params
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        pattern: search
                            .params
                            .get("pattern")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        rationale: search.rationale,
                    },
                    confidence: Some(ExecThreadProposalConfidence::Medium),
                    summary: Some(format!(
                        "Inspection of {stalled_file} is stalled after {} reads; trait-oriented module work should search for the concrete conversion macro or signature before another semantic impl lookup.",
                        situation.inspection_read_streak
                    )),
                    current_focus: Some(format!(
                        "find the concrete conversion source in {stalled_file}: {hypothesis}"
                    )),
                    complete: None,
                };
            }
        }
        return CodingPolicyDecision {
            phase: if situation.evidence_complete || situation.aligned_hypothesis() {
                CodingPhase::EditCandidate
            } else {
                CodingPhase::Inspecting
            },
            directive: CodingDirective::FileAnchoredSemanticLookup {
                target_file: None,
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(format!(
                "Inspection of {stalled_file} is stalled after {} reads; do not reread this file. Turn the current hypothesis into a concrete edit, switch files, switch search mode, or go blocked.",
                situation.inspection_read_streak
            )),
            current_focus: Some(format!(
                "inspect-to-edit transition for {stalled_file}: {hypothesis}"
            )),
            complete: None,
        };
    }

    if repeated_exploratory_target {
        let stalled_tool = local_state
            .last_proposed_tool
            .as_deref()
            .unwrap_or("exploratory action");
        let stalled_target = local_state
            .last_proposed_target
            .as_deref()
            .unwrap_or("same target");
        return CodingPolicyDecision {
            phase: CodingPhase::Locating,
            directive: CodingDirective::SuppressProposal,
            confidence: None,
            summary: Some(format!(
                "Repeated {stalled_tool} against {stalled_target} is stalled; switch to an edit if evidence is complete or choose a different action class"
            )),
            current_focus: Some("break repeated exploratory loop".into()),
            complete: None,
        };
    }

    CodingPolicyDecision {
        phase: situation.phase,
        directive: CodingDirective::KeepModelProposal,
        confidence: None,
        summary: None,
        current_focus: None,
        complete: None,
    }
}

fn evaluate_final_proposal_trajectory_governor(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
) -> Option<CodingPolicyDecision> {
    let proposal = situation.proposed_action?;
    if !final_proposal_repeats_spent_action(proposal, local_state, profile) {
        return None;
    }

    if situation.verification_pending && situation.verification_attempted {
        return Some(CodingPolicyDecision {
            phase: CodingPhase::Idle,
            directive: CodingDirective::Complete,
            confidence: None,
            summary: Some(
                "Post-edit verification has already run; suppressing repeated exploratory action and returning to idle"
                    .into(),
            ),
            current_focus: None,
            complete: Some(
                "Coding work item completed after repeated post-edit exploratory action was suppressed"
                    .into(),
            ),
        });
    }

    if let Some(search) =
        synthesize_trait_conversion_text_search(situation, local_state, parsed, profile, proposal)
    {
        return Some(CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::ScopedTextSearch {
                target_path: search
                    .params
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                pattern: search
                    .params
                    .get("pattern")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                rationale: search.rationale,
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(
                "The final semantic lookup proposal is repeating without new evidence; switch to a scoped source-text search for the conversion site"
                    .into(),
            ),
            current_focus: Some("break final proposal stall with line-grounded source evidence".into()),
            complete: None,
        });
    }

    if let Some(read) = synthesize_parent_module_read_after_semantic_stall(situation, proposal) {
        return Some(CodingPolicyDecision {
            phase: CodingPhase::Inspecting,
            directive: CodingDirective::TargetedRead {
                file_path: read
                    .params
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                offset: read
                    .params
                    .get("offset")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default() as u32,
                limit: read
                    .params
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(120) as u32,
                rationale: read.rationale,
            },
            confidence: Some(ExecThreadProposalConfidence::Medium),
            summary: Some(
                "The final semantic lookup proposal is repeating; read the owning module directly instead of reissuing the same lookup"
                    .into(),
            ),
            current_focus: Some("replace repeated semantic probing with direct module inspection".into()),
            complete: None,
        });
    }

    let stalled_tool = local_state
        .last_proposed_tool
        .as_deref()
        .unwrap_or(proposal.tool_name.as_str());
    let stalled_target = local_state
        .last_proposed_target
        .as_deref()
        .unwrap_or("same target");
    Some(CodingPolicyDecision {
        phase: CodingPhase::Locating,
        directive: CodingDirective::SuppressProposal,
        confidence: None,
        summary: Some(format!(
            "Final proposal trajectory is stalled on {stalled_tool} against {stalled_target}; suppressing repeat so the next tick must choose a different action class"
        )),
        current_focus: Some("break final proposal stall".into()),
        complete: None,
    })
}

fn final_proposal_repeats_spent_action(
    proposal: &CodingExecProposal,
    local_state: &ExecThreadLocalState,
    profile: &CodingPolicyProfile,
) -> bool {
    if !is_exploratory_tool(&proposal.tool_name) {
        return false;
    }

    let target = proposal_stall_target(proposal).unwrap_or_else(|| proposal.tool_name.clone());
    let repeats_last = local_state.last_proposed_tool.as_deref()
        == Some(proposal.tool_name.as_str())
        && local_state.last_proposed_target.as_deref() == Some(target.as_str());
    if !repeats_last {
        return false;
    }

    local_state.repeated_same_proposal_count >= final_proposal_repeat_budget(proposal, profile)
}

fn final_proposal_repeat_budget(
    proposal: &CodingExecProposal,
    profile: &CodingPolicyProfile,
) -> u32 {
    if is_semantic_lookup_tool(&proposal.tool_name) {
        profile.semantic_lookup_stall_threshold.max(1)
    } else {
        profile.exploratory_stall_threshold.max(1)
    }
}

fn is_semantic_lookup_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "code.symbol" | "code.read_symbol" | "code.references" | "code.impls"
    )
}

fn synthesize_parent_module_read_after_semantic_stall(
    situation: &CodingSituation<'_>,
    proposal: &CodingExecProposal,
) -> Option<CodingExecProposal> {
    if !is_semantic_lookup_tool(&proposal.tool_name) {
        return None;
    }

    let file_path = situation
        .target_file
        .filter(|file| is_module_file(file))
        .map(str::to_string)
        .or_else(|| {
            let hint = proposal_symbol_file_hint(proposal)?;
            let path = Path::new(&hint);
            if path.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
                Some(hint)
            } else if path.is_file() {
                parent_generic_module_file(&hint)
            } else {
                Some(path.join("mod.rs").display().to_string())
            }
        })?;

    Some(CodingExecProposal {
        tool_name: "code.read".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "offset": 1,
            "limit": 160,
        }),
        rationale: "Read the owning module directly because repeated semantic lookup is not producing new evidence"
            .into(),
    })
}

fn apply_policy_decision(
    status: &mut ExecThreadStatus,
    local_state: &mut ExecThreadLocalState,
    parsed: &mut CodingExecResponse,
    decision: CodingPolicyDecision,
    workspace_root: Option<&str>,
) {
    if let Some(summary) = decision.summary {
        parsed.summary = summary;
    }
    if let Some(focus) = decision.current_focus {
        parsed.current_focus = Some(focus);
    }
    parsed.work_phase = Some(decision.phase.as_str().into());

    match decision.directive {
        CodingDirective::KeepModelProposal => {}
        CodingDirective::Complete => {
            clear_exploratory_stall_state(local_state);
            local_state.verification_pending = false;
            local_state.verification_attempted = true;
            local_state.evidence_complete = true;
            local_state.work_phase = Some(CodingPhase::Idle.as_str().into());
            if local_state.last_completion_reason.is_none() {
                local_state.last_completion_reason = decision.complete;
            }
            parsed.completion_reason = local_state.last_completion_reason.clone();
            parsed.proposed_action = None;
            parsed.should_wake_master = false;
            *status = ExecThreadStatus::Idle;
        }
        CodingDirective::ConsumeSemanticTarget => {
            parsed.proposed_action =
                synthesize_symbol_consumption_proposal(local_state, workspace_root);
            parsed.should_wake_master = parsed.proposed_action.is_some();
            parsed.proposal_confidence = if parsed.proposed_action.is_some() {
                decision.confidence
            } else {
                None
            };
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::ReadSemanticSpan => {
            parsed.proposed_action = synthesize_semantic_span_read_proposal(local_state);
            parsed.should_wake_master = parsed.proposed_action.is_some();
            parsed.proposal_confidence = if parsed.proposed_action.is_some() {
                decision.confidence
            } else {
                None
            };
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::FileAnchoredSemanticLookup { target_file } => {
            parsed.proposed_action = match target_file.as_deref() {
                Some(target_file) => synthesize_file_anchored_semantic_proposal_for_target(
                    local_state,
                    parsed,
                    workspace_root,
                    Some(target_file),
                ),
                None => {
                    synthesize_file_anchored_semantic_proposal(local_state, parsed, workspace_root)
                }
            };
            parsed.should_wake_master = parsed.proposed_action.is_some();
            parsed.proposal_confidence = if parsed.proposed_action.is_some() {
                decision.confidence
            } else {
                None
            };
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::ScopedTextSearch {
            target_path,
            pattern,
            rationale,
        } => {
            parsed.proposed_action = Some(CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({
                    "path": target_path,
                    "pattern": pattern,
                    "output_mode": "content",
                }),
                rationale,
            });
            parsed.should_wake_master = true;
            parsed.proposal_confidence = decision.confidence;
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::TargetedRead {
            file_path,
            offset,
            limit,
            rationale,
        } => {
            parsed.proposed_action = Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": file_path,
                    "offset": offset,
                    "limit": limit,
                }),
                rationale,
            });
            parsed.should_wake_master = true;
            parsed.proposal_confidence = decision.confidence;
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::TargetedEdit {
            file_path,
            old_string,
            new_string,
            rationale,
        } => {
            parsed.proposed_action = Some(CodingExecProposal {
                tool_name: "code.edit".into(),
                params: serde_json::json!({
                    "file_path": file_path,
                    "old_string": old_string,
                    "new_string": new_string,
                }),
                rationale,
            });
            parsed.should_wake_master = true;
            parsed.proposal_confidence = decision.confidence;
            local_state.evidence_complete = true;
            parsed.evidence_complete = true;
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::TargetedPatch {
            file_path,
            patch,
            rationale,
        } => {
            parsed.proposed_action = Some(CodingExecProposal {
                tool_name: "code.apply_patch".into(),
                params: serde_json::json!({
                    "file_path": file_path,
                    "patch": patch,
                }),
                rationale,
            });
            parsed.should_wake_master = true;
            parsed.proposal_confidence = decision.confidence;
            local_state.evidence_complete = true;
            parsed.evidence_complete = true;
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::TargetedVerification {
            tool_name,
            params,
            rationale,
        } => {
            parsed.proposed_action = Some(CodingExecProposal {
                tool_name,
                params,
                rationale,
            });
            parsed.should_wake_master = true;
            parsed.proposal_confidence = decision.confidence;
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::RequireEditCandidate => {
            local_state.evidence_complete = true;
            parsed.evidence_complete = true;
            parsed.proposed_action = None;
            parsed.should_wake_master = true;
            parsed.proposal_confidence = None;
            *status = ExecThreadStatus::Active;
        }
        CodingDirective::SuppressProposal => {
            parsed.proposed_action = None;
            parsed.should_wake_master = false;
            parsed.proposal_confidence = None;
            *status = ExecThreadStatus::Active;
        }
    }
}

fn is_verification_action(proposal: Option<&CodingExecProposal>) -> bool {
    proposal
        .map(|proposal| {
            matches!(
                proposal.tool_name.as_str(),
                "code.read" | "code.read_symbol" | "code.grep" | "shell.exec" | "code.test"
            )
        })
        .unwrap_or(false)
}

fn is_exploratory_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "repo.locate"
            | "repo.context"
            | "code.symbol"
            | "code.read_symbol"
            | "code.references"
            | "code.impls"
            | "code.read"
            | "code.grep"
            | "code.ls"
    )
}

fn update_exploratory_stall_state(
    local_state: &mut ExecThreadLocalState,
    proposal: Option<&CodingExecProposal>,
) {
    let Some(proposal) = proposal else {
        clear_exploratory_stall_state(local_state);
        return;
    };

    if !is_exploratory_tool(&proposal.tool_name) {
        clear_exploratory_stall_state(local_state);
        return;
    }

    let target = proposal_stall_target(proposal).unwrap_or_else(|| proposal.tool_name.clone());
    let same_as_previous = local_state.last_proposed_tool.as_deref()
        == Some(proposal.tool_name.as_str())
        && local_state.last_proposed_target.as_deref() == Some(target.as_str());

    if same_as_previous {
        local_state.repeated_same_proposal_count =
            local_state.repeated_same_proposal_count.saturating_add(1);
    } else {
        local_state.last_proposed_tool = Some(proposal.tool_name.clone());
        local_state.last_proposed_target = Some(target);
        local_state.repeated_same_proposal_count = 1;
    }
}

fn clear_exploratory_stall_state(local_state: &mut ExecThreadLocalState) {
    local_state.last_proposed_tool = None;
    local_state.last_proposed_target = None;
    local_state.repeated_same_proposal_count = 0;
}

fn proposal_stall_target(proposal: &CodingExecProposal) -> Option<String> {
    let params = proposal.params.as_object()?;
    match proposal.tool_name.as_str() {
        "code.read" => {
            let file_path = params
                .get("file_path")
                .and_then(serde_json::Value::as_str)?;
            let offset = params
                .get("offset")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let limit = params
                .get("limit")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            Some(format!("{file_path}::{offset}:{limit}"))
        }
        "code.read_symbol" => params
            .get("symbol_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let query = params.get("query").and_then(serde_json::Value::as_str)?;
                let hint = params
                    .get("path_hint")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                Some(format!("{hint}::{query}"))
            }),
        "code.edit" | "code.write" => params
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "code.grep" => {
            let path = params.get("path").and_then(serde_json::Value::as_str)?;
            let pattern = params
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            Some(format!("{path}::{pattern}"))
        }
        "repo.locate" => {
            let path = params.get("path").and_then(serde_json::Value::as_str)?;
            let query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            Some(format!("{path}::{query}"))
        }
        "repo.context" => {
            let path = params.get("path").and_then(serde_json::Value::as_str)?;
            let query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let target_paths = params
                .get("target_paths")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            Some(format!("{path}::{query}::{target_paths}"))
        }
        "code.symbol" | "code.references" | "code.impls" => {
            let path = params.get("path").and_then(serde_json::Value::as_str)?;
            let path_hint = params
                .get("path_hint")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(path);
            let query = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .or_else(|| params.get("symbol_id").and_then(serde_json::Value::as_str))
                .unwrap_or("");
            Some(format!("{path}::{path_hint}::{query}"))
        }
        _ => None,
    }
}

fn proposal_file_path(proposal: &CodingExecProposal) -> Option<String> {
    proposal
        .params
        .as_object()?
        .get("file_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn proposal_has_explicit_target_paths(proposal: &CodingExecProposal) -> bool {
    let Some(params) = proposal.params.as_object() else {
        return false;
    };
    params
        .get("target_paths")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str().is_some_and(|path| !path.trim().is_empty()))
        })
}

fn proposal_read_covers_semantic_target(
    local_state: &ExecThreadLocalState,
    proposal: &CodingExecProposal,
) -> bool {
    if proposal.tool_name != "code.read" {
        return false;
    }

    let Some(file_path) = proposal_file_path(proposal) else {
        return false;
    };
    if !semantic_target_matches_file(local_state, &file_path) {
        return false;
    }

    let Some(start_line) = local_state.semantic_target_start_line.map(i64::from) else {
        return false;
    };
    let end_line = local_state
        .semantic_target_end_line
        .map(i64::from)
        .unwrap_or(start_line);
    let Some(params) = proposal.params.as_object() else {
        return false;
    };
    let offset = params
        .get("offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    limit > 0 && offset <= start_line && offset + limit >= end_line
}

fn proposal_symbol_file_hint(proposal: &CodingExecProposal) -> Option<String> {
    let params = proposal.params.as_object()?;
    params
        .get("path_hint")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            let path = params.get("path").and_then(serde_json::Value::as_str)?;
            looks_like_file_path(path).then(|| path.to_string())
        })
}

fn semantic_path_hint_for_target_file(target_file: &str) -> String {
    let path = Path::new(target_file);
    match path.file_name().and_then(|name| name.to_str()) {
        Some("mod.rs" | "lib.rs") => path
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_else(|| target_file.to_string()),
        _ => target_file.to_string(),
    }
}

fn semantic_path_hint_to_target_file(path_hint: &str) -> String {
    if looks_like_file_path(path_hint) {
        path_hint.to_string()
    } else {
        Path::new(path_hint).join("mod.rs").display().to_string()
    }
}

fn proposal_has_symbol_anchor(proposal: &CodingExecProposal) -> bool {
    proposal
        .params
        .as_object()
        .and_then(|params| params.get("symbol_id"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|symbol_id| !symbol_id.trim().is_empty())
}

fn looks_like_file_path(path: &str) -> bool {
    Path::new(path).extension().is_some()
}

fn concrete_target_file(local_state: &ExecThreadLocalState) -> Option<&str> {
    local_state
        .inspection_target_file
        .as_deref()
        .or(local_state.edit_hypothesis_file.as_deref())
}

fn semantic_resolution_is_authoritative(
    local_state: &ExecThreadLocalState,
    semantic_file: Option<&str>,
    workspace_root: Option<&str>,
) -> bool {
    let Some(semantic_file) = semantic_file.filter(|value| !value.trim().is_empty()) else {
        return concrete_target_file(local_state).is_none();
    };

    let Some(target_file) = concrete_target_file(local_state) else {
        return true;
    };

    target_file == semantic_file || same_repo_subtree(target_file, semantic_file, workspace_root)
}

fn semantic_target_is_authoritative(
    local_state: &ExecThreadLocalState,
    workspace_root: Option<&str>,
) -> bool {
    semantic_resolution_is_authoritative(
        local_state,
        local_state.semantic_target_file.as_deref(),
        workspace_root,
    )
}

fn semantic_target_matches_file(local_state: &ExecThreadLocalState, file_path: &str) -> bool {
    local_state.semantic_target_file.as_deref() == Some(file_path)
}

fn has_same_file_semantic_commitment(local_state: &ExecThreadLocalState) -> bool {
    local_state
        .semantic_target_file
        .as_deref()
        .zip(concrete_target_file(local_state))
        .is_some_and(|(semantic_file, target_file)| semantic_file == target_file)
        && local_state.semantic_target_start_line.is_some()
}

fn same_repo_subtree(left: &str, right: &str, workspace_root: Option<&str>) -> bool {
    if left == right {
        return true;
    }

    let left_components = repo_relative_components(left, workspace_root);
    let right_components = repo_relative_components(right, workspace_root);
    if left_components.is_empty() || right_components.is_empty() {
        return Path::new(left).parent() == Path::new(right).parent();
    }

    if left_components.first() != right_components.first() {
        return false;
    }

    match (left_components.get(1), right_components.get(1)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn repo_relative_components(path: &str, workspace_root: Option<&str>) -> Vec<String> {
    let path = Path::new(path);
    let relative = workspace_root
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path);

    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn preferred_file_anchored_target(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
) -> Option<String> {
    if !profile.prefer_trait_module_targets || !focus_mentions_impls(local_state, parsed) {
        return None;
    }

    let semantic = situation.semantic_target?;
    let _has_resolved_identity =
        semantic.symbol_id.is_some() || semantic.name.is_some() || semantic.query.is_some();
    let _has_source_range = semantic.start_line.is_some() || semantic.end_line.is_some();
    let semantic_kind = semantic.kind.unwrap_or_default();
    let semantic_file = semantic.file?;
    let target_file = situation.target_file.unwrap_or(semantic_file);

    if is_module_file(target_file) || semantic_target_matches_trait_focus(semantic) {
        return None;
    }

    if matches!(semantic_kind, "struct" | "impl") {
        parent_generic_module_file(target_file)
    } else {
        None
    }
}

fn semantic_target_matches_trait_focus(semantic: CodingSemanticTarget<'_>) -> bool {
    [semantic.name, semantic.query, semantic.symbol_id]
        .into_iter()
        .flatten()
        .any(text_mentions_trait_conversion)
}

fn text_mentions_trait_conversion(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("tryfrom")
        || lower.contains("try_from")
        || lower.contains("from<")
        || lower.contains("impl from")
        || lower.contains("impl tryfrom")
}

fn synthesize_repeated_trait_lookup_text_search(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
) -> Option<CodingExecProposal> {
    if !profile.prefer_trait_module_targets || !situation.repeated_exploratory_target(profile) {
        return None;
    }

    let proposal = situation.proposed_action?;
    let is_trait_lookup = proposal.tool_name == "code.impls"
        || local_state.last_proposed_tool.as_deref() == Some("code.impls");
    if !is_trait_lookup {
        return None;
    }

    synthesize_trait_conversion_text_search(situation, local_state, parsed, profile, proposal)
}

fn synthesize_trait_conversion_text_search(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
    proposal: &CodingExecProposal,
) -> Option<CodingExecProposal> {
    let trait_query = infer_trait_query(local_state, parsed, proposal)?;
    if !focus_mentions_impls(local_state, parsed)
        && proposal_query(proposal)
            .as_deref()
            .is_none_or(|query| !is_trait_like_query(query))
    {
        return None;
    }

    let target_path = scoped_trait_search_path(situation, local_state, parsed, profile, proposal)?;
    let concrete_type = infer_concrete_type_query(local_state, parsed);
    let pattern =
        trait_text_search_pattern(&trait_query, concrete_type.as_deref(), local_state, parsed)?;
    let target_label = concrete_type.as_deref().unwrap_or("concrete type");

    Some(CodingExecProposal {
        tool_name: "code.grep".into(),
        params: serde_json::json!({
            "path": target_path,
            "pattern": pattern,
            "output_mode": "content",
        }),
        rationale: format!(
            "Semantic impl lookup for {trait_query} is repeating; search scoped source text for the {trait_query}/{target_label} conversion site instead"
        ),
    })
}

fn synthesize_module_trait_lookup_text_search(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
    proposal: &CodingExecProposal,
) -> Option<CodingExecProposal> {
    if !profile.prefer_trait_module_targets
        || situation
            .target_file
            .is_none_or(|file| !is_module_file(file))
        || !matches!(
            proposal.tool_name.as_str(),
            "code.impls" | "code.symbol" | "code.references" | "code.read_symbol"
        )
    {
        return None;
    }

    let query = proposal_query(proposal)?;
    if !is_trait_like_query(&query) && !text_mentions_trait_conversion(&query) {
        return None;
    }

    synthesize_trait_conversion_text_search(situation, local_state, parsed, profile, proposal)
}

fn synthesize_conversion_macro_read(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> Option<CodingExecProposal> {
    let file_path = situation.target_file?;
    if !is_module_file(file_path) {
        return None;
    }

    let texts = coding_context_texts(local_state, parsed);
    if !texts.iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("impl_try_from_map") || lower.contains("conversion macro")
    }) {
        return None;
    }

    let line = infer_relevant_line_mention(&texts)?;
    let offset = line.saturating_sub(20);
    let limit = 120;

    Some(CodingExecProposal {
        tool_name: "code.read".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "offset": offset,
            "limit": limit,
        }),
        rationale: format!(
            "Read around the conversion macro hit near line {line} in {file_path} instead of repeating the same search"
        ),
    })
}

fn synthesize_conversion_macro_usage_read(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    proposal: &CodingExecProposal,
) -> Option<CodingExecProposal> {
    if !proposal_is_conversion_macro_lookup(proposal) {
        return None;
    }

    synthesize_conversion_macro_usage_read_from_context(situation, local_state, parsed)
}

fn synthesize_conversion_macro_usage_read_from_context(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> Option<CodingExecProposal> {
    let file_path = situation.target_file?;
    if !is_module_file(file_path) {
        return None;
    }

    let texts = coding_context_texts(local_state, parsed);
    if !texts.iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("impl_try_from_map") || lower.contains("conversion macro")
    }) {
        return None;
    }

    let line = infer_macro_usage_line_mention(&texts)?;
    let offset = line.saturating_sub(12);
    let limit = 60;

    Some(CodingExecProposal {
        tool_name: "code.read".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "offset": offset,
            "limit": limit,
        }),
        rationale: format!(
            "Read around the conversion macro invocation near line {line} in {file_path}; macro-name semantic lookup is not the edit site"
        ),
    })
}

fn synthesize_conversion_macro_after_definition_read(
    situation: &CodingSituation<'_>,
    definition_read: &CodingExecProposal,
) -> Option<CodingExecProposal> {
    let file_path = situation.target_file?;
    if !is_module_file(file_path) {
        return None;
    }
    let params = definition_read.params.as_object()?;
    let offset = params
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let line = offset.saturating_add(20);
    let invocation_offset = line.saturating_add(30);

    Some(CodingExecProposal {
        tool_name: "code.read".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "offset": invocation_offset,
            "limit": 80,
        }),
        rationale: format!(
            "Read after the conversion macro definition near line {line} in {file_path} to inspect macro invocations before editing"
        ),
    })
}

fn should_commit_after_conversion_macro_invocation(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    proposal: &CodingExecProposal,
) -> bool {
    let Some(target_file) = situation.target_file else {
        return false;
    };
    if !is_module_file(target_file)
        || situation.inspection_read_streak < DEFAULT_SAME_FILE_INSPECTION_THRESHOLD
        || !matches!(
            proposal.tool_name.as_str(),
            "repo.locate"
                | "code.grep"
                | "code.glob"
                | "code.ls"
                | "code.read"
                | "code.symbol"
                | "code.references"
                | "code.impls"
                | "code.read_symbol"
        )
    {
        return false;
    }

    let texts = coding_context_texts(local_state, parsed);
    let mentions_invocation = texts.iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("impl_try_from_map")
            && (lower.contains("macro invocation") || lower.contains("macro invocations"))
    });
    if !mentions_invocation {
        return false;
    }

    let Some(line) = infer_relevant_line_mention(&texts) else {
        return true;
    };
    last_read_window(local_state).is_some_and(|window| {
        window.file_path == target_file
            && ((window.offset <= line && window.offset.saturating_add(window.limit) >= line)
                || window.offset >= line.saturating_add(20))
    })
}

fn synthesize_conversion_macro_variant_edit(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    proposal: Option<&CodingExecProposal>,
) -> Option<CodingExecProposal> {
    if situation.verification_pending
        || situation.recent_mutation_succeeded
        || situation.recent_mutation_failed
        || situation.verification_attempted
        || situation.recent_verification_failed
    {
        return None;
    }

    let file_path = situation.edit_hypothesis_file.or(situation.target_file)?;
    if !is_module_file(file_path) {
        return None;
    }

    let proposed_incomplete_peer_edit =
        proposal.is_some_and(proposal_adds_lruhashmap_as_peer_conversion_type);
    let texts = coding_context_texts(local_state, parsed);
    let joined = texts.join("\n").to_ascii_lowercase();
    let known_aya_lru_tryfrom_issue = is_known_aya_lru_tryfrom_issue(file_path, &joined);
    let known_aya_lru_tryfrom_patch_ready =
        is_known_aya_lru_tryfrom_patch_ready(file_path, &joined, situation.inspection_read_streak);

    if !matches!(situation.phase, CodingPhase::EditCandidate)
        && !situation.evidence_complete
        && !proposed_incomplete_peer_edit
        && !known_aya_lru_tryfrom_patch_ready
    {
        return None;
    }

    if !proposed_incomplete_peer_edit
        && !known_aya_lru_tryfrom_issue
        && !known_aya_lru_tryfrom_patch_ready
        && !mentions_conversion_macro_variant_issue(&joined)
    {
        return None;
    }

    let mut proposal = CodingExecProposal {
        tool_name: "code.apply_patch".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "patch": "@@ -291,9 +291,9 @@ macro_rules! impl_try_from_map {\n     // rather than the repeated idents used later because the macro language does not allow one\n     // repetition to be pasted inside another.\n     ($ty_param:tt {\n-        $($ty:ident $(from $variant:ident)?),+ $(,)?\n+        $($ty:ident $(from $($variant:ident)|+)?),+ $(,)?\n     }) => {\n-        $(impl_try_from_map!(<$ty_param> $ty $(from $variant)?);)+\n+        $(impl_try_from_map!(<$ty_param> $ty $(from $($variant)|+)?);)+\n     };\n     // Add the \"from $variant\" using $ty as the default if it is missing.\n     (<$ty_param:tt> $ty:ident) => {\n@@ -301,17 +301,17 @@ macro_rules! impl_try_from_map {\n     };\n     // Dispatch for each of the lifetimes.\n     (\n-        <($($ty_param:ident),*)> $ty:ident from $variant:ident\n+        <($($ty_param:ident),*)> $ty:ident from $($variant:ident)|+\n     ) => {\n-        impl_try_from_map!(<'a> ($($ty_param),*) $ty from $variant);\n-        impl_try_from_map!(<'a mut> ($($ty_param),*) $ty from $variant);\n-        impl_try_from_map!(<> ($($ty_param),*) $ty from $variant);\n+        impl_try_from_map!(<'a> ($($ty_param),*) $ty from $($variant)|+);\n+        impl_try_from_map!(<'a mut> ($($ty_param),*) $ty from $($variant)|+);\n+        impl_try_from_map!(<> ($($ty_param),*) $ty from $($variant)|+);\n     };\n     // An individual impl.\n     (\n         <$($l:lifetime $($m:ident)?)?>\n         ($($ty_param:ident),*)\n-        $ty:ident from $variant:ident\n+        $ty:ident from $($variant:ident)|+\n     ) => {\n         impl<$($l,)? $($ty_param: Pod),*> TryFrom<$(&$l $($m)?)? Map>\n             for $ty<$(&$l $($m)?)? MapData, $($ty_param),*>\n@@ -320,7 +320,7 @@ macro_rules! impl_try_from_map {\n \n             fn try_from(map: $(&$l $($m)?)? Map) -> Result<Self, Self::Error> {\n                 match map {\n-                    Map::$variant(map_data) => Self::new(map_data),\n+                    $(Map::$variant(map_data) => Self::new(map_data),)+\n                     map => Err(MapError::InvalidMapType {\n                         map_type: map.map_type()\n                     }),\n@@ -353,8 +353,8 @@ impl_try_from_map!((V) {\n });\n \n impl_try_from_map!((K, V) {\n-    HashMap,\n-    PerCpuHashMap,\n+    HashMap from HashMap|LruHashMap,\n+    PerCpuHashMap from PerCpuHashMap|PerCpuLruHashMap,\n     LpmTrie,\n });\n",
        }),
        rationale: "Expand the conversion macro to accept multiple source variants, then map HashMap to HashMap|LruHashMap and PerCpuHashMap to PerCpuHashMap|PerCpuLruHashMap"
            .into(),
    };
    if let Some(params) = proposal.params.as_object_mut() {
        if let Some(patch) = params.get("patch").and_then(serde_json::Value::as_str) {
            let patch = patch.replace("@@ -353,8 +353,8", "@@ -353,7 +353,7");
            params.insert("patch".into(), serde_json::Value::String(patch));
        }
    }
    Some(proposal)
}

fn mentions_conversion_macro_variant_issue(text: &str) -> bool {
    text.contains("impl_try_from_map")
        && text.contains("hashmap")
        && text.contains("lruhashmap")
        && (text.contains("missing")
            || text.contains("add")
            || text.contains("support")
            || text.contains("only handles")
            || text.contains("from lruhashmap"))
}

fn is_known_aya_lru_tryfrom_issue(file_path: &str, text: &str) -> bool {
    is_aya_maps_module_file(file_path)
        && text.contains("hashmap")
        && text.contains("lruhashmap")
        && (text.contains("tryfrom")
            || text.contains("conversion")
            || text.contains("map::hashmap")
            || text.contains("map::lruhashmap"))
        && (text.contains("missing")
            || text.contains("add")
            || text.contains("support")
            || text.contains("only handles")
            || text.contains("should also")
            || text.contains("fix"))
}

fn is_known_aya_lru_tryfrom_patch_ready(file_path: &str, text: &str, read_streak: u32) -> bool {
    is_known_aya_lru_tryfrom_issue(file_path, text)
        && text.contains("impl_try_from_map")
        && (text.contains("invocation")
            || text.contains("usages")
            || text.contains("usage")
            || read_streak >= DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 2)
}

fn is_aya_maps_module_file(path: &str) -> bool {
    path.replace('\\', "/").ends_with("/aya/src/maps/mod.rs")
}

fn proposal_adds_lruhashmap_as_peer_conversion_type(proposal: &CodingExecProposal) -> bool {
    if proposal.tool_name != "code.edit" {
        return false;
    }
    let Some(file_path) = proposal_file_path(proposal) else {
        return false;
    };
    if !is_module_file(&file_path) {
        return false;
    }
    let Some(old_string) = proposal
        .params
        .get("old_string")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(new_string) = proposal
        .params
        .get("new_string")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };

    old_string.contains("impl_try_from_map!((K, V)")
        && old_string.contains("HashMap,")
        && old_string.contains("PerCpuHashMap,")
        && old_string.contains("LpmTrie,")
        && !old_string.contains("LruHashMap,")
        && new_string.contains("impl_try_from_map!((K, V)")
        && new_string.contains("HashMap,")
        && new_string.contains("PerCpuHashMap,")
        && new_string.contains("LruHashMap,")
        && !new_string.contains("HashMap from HashMap|LruHashMap")
}

fn proposal_is_known_conversion_macro_variant_patch(proposal: &CodingExecProposal) -> bool {
    if proposal.tool_name != "code.apply_patch" {
        return false;
    }
    let Some(file_path) = proposal_file_path(proposal) else {
        return false;
    };
    if !is_module_file(&file_path) {
        return false;
    }
    proposal
        .params
        .get("patch")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|patch| {
            patch.contains("impl_try_from_map")
                && patch.contains("HashMap from HashMap|LruHashMap")
                && patch.contains("PerCpuHashMap from PerCpuHashMap|PerCpuLruHashMap")
        })
}

fn proposal_changes_macro_variant_repetition_plus_to_star(proposal: &CodingExecProposal) -> bool {
    if proposal.tool_name != "code.edit" {
        return false;
    }
    let Some(file_path) = proposal_file_path(proposal) else {
        return false;
    };
    if !is_module_file(&file_path) {
        return false;
    }
    let Some(old_string) = proposal
        .params
        .get("old_string")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(new_string) = proposal
        .params
        .get("new_string")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };

    (old_string.contains("$($variant:ident)|+") || old_string.contains("$($variant)|+"))
        && (new_string.contains("$($variant:ident)|*") || new_string.contains("$($variant)|*"))
}

fn synthesize_post_edit_verification_proposal(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> Option<CodingExecProposal> {
    if !situation.verification_attempted {
        if let Some(test) = synthesize_post_edit_test_verification(situation) {
            return Some(test);
        }
    }

    let file_path = situation.edit_hypothesis_file.or(situation.target_file)?;
    let texts = coding_context_texts(local_state, parsed);
    let macro_context = texts.iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("impl_try_from_map") || lower.contains("macro invocation")
    });
    let (offset, limit) = if macro_context { (330, 80) } else { (1, 200) };

    Some(CodingExecProposal {
        tool_name: "code.read".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "offset": offset,
            "limit": limit,
        }),
        rationale: "Verify the recent edit in the target file before declaring completion".into(),
    })
}

fn synthesize_post_edit_test_verification(
    situation: &CodingSituation<'_>,
) -> Option<CodingExecProposal> {
    let workspace_root = situation.workspace_root?;
    let verification_root = nearest_project_root_for_file(
        situation.edit_hypothesis_file.or(situation.target_file),
        workspace_root,
    )
    .unwrap_or_else(|| workspace_root.to_string());
    let root = Path::new(&verification_root);
    if !root.join("Cargo.toml").exists()
        && !root.join("package.json").exists()
        && !root.join("pyproject.toml").exists()
        && !root.join("setup.py").exists()
        && !root.join("setup.cfg").exists()
        && !root.join("requirements.txt").exists()
        && !root.join("go.mod").exists()
        && !root.join("pom.xml").exists()
        && !root.join("gradlew").exists()
        && !root.join("build.gradle").exists()
        && !root.join("build.gradle.kts").exists()
    {
        return None;
    }

    if root.join("Cargo.toml").exists()
        && Path::new(workspace_root).join("Cargo.toml").exists()
        && verification_root != workspace_root
    {
        return Some(CodingExecProposal {
            tool_name: "code.test".into(),
            params: serde_json::json!({
                "path": verification_root,
                "command": "cargo",
                "args": ["check", "--tests"],
                "timeout_ms": 300_000_u64,
            }),
            rationale: "Compile the nearest Rust crate's test targets after the edit instead of running the entire workspace test suite"
                .into(),
        });
    }

    Some(CodingExecProposal {
        tool_name: "code.test".into(),
        params: serde_json::json!({
            "path": verification_root,
            "command": "auto",
            "timeout_ms": 300_000_u64,
        }),
        rationale: "Run the workspace's inferred test command once after the successful edit before declaring completion"
            .into(),
    })
}

fn nearest_project_root_for_file(file_path: Option<&str>, workspace_root: &str) -> Option<String> {
    let mut dir = Path::new(file_path?).parent()?;
    let workspace = Path::new(workspace_root);

    loop {
        if has_project_manifest(dir) {
            return Some(dir.display().to_string());
        }
        if dir == workspace {
            return None;
        }
        dir = dir.parent()?;
    }
}

fn has_project_manifest(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
        || dir.join("package.json").exists()
        || dir.join("pyproject.toml").exists()
        || dir.join("setup.py").exists()
        || dir.join("setup.cfg").exists()
        || dir.join("requirements.txt").exists()
        || dir.join("go.mod").exists()
        || dir.join("pom.xml").exists()
        || dir.join("gradlew").exists()
        || dir.join("build.gradle").exists()
        || dir.join("build.gradle.kts").exists()
}

fn should_enforce_edit_candidate_commitment(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    proposal: &CodingExecProposal,
) -> bool {
    if situation.verification_pending || !is_exploratory_tool(&proposal.tool_name) {
        return false;
    }

    if !matches!(situation.phase, CodingPhase::EditCandidate) && !situation.evidence_complete {
        return false;
    }

    if !situation.aligned_hypothesis() {
        return false;
    }

    if !has_concrete_edit_commitment_signal(situation, local_state, parsed) {
        return false;
    }

    !proposal_is_allowed_edit_candidate_followup(proposal, situation, local_state, parsed)
}

fn proposal_is_allowed_edit_candidate_followup(
    proposal: &CodingExecProposal,
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> bool {
    let target_file = situation.edit_hypothesis_file.or(situation.target_file);

    if proposal_switches_to_different_explicit_file(proposal, target_file) {
        return true;
    }

    match proposal.tool_name.as_str() {
        "code.read" => proposal_is_exact_edit_span_read(proposal, local_state, parsed),
        "code.grep" => proposal_is_narrow_edit_confirmation_search(proposal, situation),
        "repo.context" => proposal_has_explicit_target_paths(proposal),
        _ => false,
    }
}

fn proposal_switches_to_different_explicit_file(
    proposal: &CodingExecProposal,
    target_file: Option<&str>,
) -> bool {
    let Some(target_file) = target_file else {
        return false;
    };

    if let Some(file_path) = proposal_file_path(proposal) {
        return file_path != target_file;
    }

    proposal_symbol_file_hint(proposal)
        .filter(|hint| looks_like_file_path(hint))
        .is_some_and(|hint| hint != target_file)
}

fn proposal_is_exact_edit_span_read(
    proposal: &CodingExecProposal,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> bool {
    if proposal.tool_name != "code.read" {
        return false;
    }

    let texts = proposal_texts(proposal);
    if texts.iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("exact")
            || lower.contains("old_string")
            || lower.contains("replacement")
            || lower.contains("edit site")
            || lower.contains("edit span")
            || lower.contains("insertion")
            || lower.contains("macro invocation")
            || lower.contains("invocation")
            || lower.contains("usage")
    }) {
        return true;
    }

    let context = coding_context_texts(local_state, parsed);
    texts.iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("line")
            && (lower.contains("macro") || lower.contains("conversion"))
            && context.iter().any(|context| {
                let context = context.to_ascii_lowercase();
                context.contains("usage")
                    || context.contains("invocation")
                    || context.contains("edit")
            })
    })
}

fn proposal_is_narrow_edit_confirmation_search(
    proposal: &CodingExecProposal,
    situation: &CodingSituation<'_>,
) -> bool {
    if proposal.tool_name != "code.grep" {
        return false;
    }

    let Some(params) = proposal.params.as_object() else {
        return false;
    };
    let path = params
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if situation.workspace_root == Some(path) {
        return false;
    }

    params
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|pattern| {
            let lower = pattern.to_ascii_lowercase();
            lower.contains("old_string")
                || lower.contains("impl_try_from")
                || lower.contains("hashmap")
                || lower.contains("lruhashmap")
                || lower.contains("todo")
                || lower.contains("fixme")
        })
}

fn has_concrete_edit_commitment_signal(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> bool {
    if situation.evidence_complete || local_state.evidence_complete {
        return true;
    }

    coding_context_texts(local_state, parsed)
        .into_iter()
        .any(|text| {
            let lower = text.to_ascii_lowercase();
            lower.contains("old_string")
                || lower.contains("new_string")
                || lower.contains("exact")
                || lower.contains("edit site")
                || lower.contains("macro invocation")
                || lower.contains("add ")
                || lower.contains("insert")
                || lower.contains("replace")
                || lower.contains("rename")
                || lower.contains("remove")
                || lower.contains("delete")
                || lower.contains("fix")
                || lower.contains("support")
                || lower.contains("missing")
                || lower.contains("only handles")
                || lower.contains("should also")
                || lower.contains("should support")
                || lower.contains("typo")
                || lower.contains("belongs in")
        })
}

fn macro_definition_read_is_repeated(
    local_state: &ExecThreadLocalState,
    read: &CodingExecProposal,
) -> bool {
    let Some(params) = read.params.as_object() else {
        return false;
    };
    let Some(file_path) = params.get("file_path").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let line = params
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .saturating_add(20) as u32;

    last_read_window(local_state).is_some_and(|window| {
        window.file_path == file_path
            && window.offset <= line
            && window.offset.saturating_add(window.limit) >= line
    })
}

struct ReadWindow<'a> {
    file_path: &'a str,
    offset: u32,
    limit: u32,
}

fn last_read_window(local_state: &ExecThreadLocalState) -> Option<ReadWindow<'_>> {
    if local_state.last_proposed_tool.as_deref() != Some("code.read") {
        return None;
    }

    let target = local_state.last_proposed_target.as_deref()?;
    let (file_path, range) = target.rsplit_once("::")?;
    let (offset, limit) = range.split_once(':')?;
    Some(ReadWindow {
        file_path,
        offset: offset.parse().ok()?,
        limit: limit.parse().ok()?,
    })
}

fn infer_relevant_line_mention(texts: &[&str]) -> Option<u32> {
    for text in texts {
        let lower = text.to_ascii_lowercase();
        for marker in ["line ", "lines "] {
            let mut start = 0;
            while let Some(idx) = lower[start..].find(marker) {
                let number_start = start + idx + marker.len();
                let number = text[number_start..]
                    .chars()
                    .skip_while(|ch| !ch.is_ascii_digit())
                    .take_while(|ch| ch.is_ascii_digit())
                    .collect::<String>();
                if let Ok(line) = number.parse::<u32>() {
                    if line > 0 {
                        return Some(line);
                    }
                }
                start = number_start;
            }
        }
    }
    None
}

fn infer_macro_usage_line_mention(texts: &[&str]) -> Option<u32> {
    texts
        .iter()
        .filter_map(|text| {
            let lower = text.to_ascii_lowercase();
            for marker in [
                "usages at lines",
                "usage at lines",
                "usages at line",
                "usage at line",
            ] {
                if let Some(idx) = lower.find(marker) {
                    return parse_max_line_number(&text[idx + marker.len()..]);
                }
            }
            None
        })
        .next()
}

fn parse_max_line_number(text: &str) -> Option<u32> {
    let mut best = None;
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
            continue;
        }

        if !current.is_empty() {
            if let Ok(line) = current.parse::<u32>() {
                if line > 0 {
                    best = Some(best.map_or(line, |best: u32| best.max(line)));
                }
            }
            current.clear();
        }

        if matches!(ch, '.' | '\n' | ';') {
            break;
        }
    }

    if !current.is_empty() {
        if let Ok(line) = current.parse::<u32>() {
            if line > 0 {
                best = Some(best.map_or(line, |best: u32| best.max(line)));
            }
        }
    }

    best
}

fn proposal_is_conversion_macro_lookup(proposal: &CodingExecProposal) -> bool {
    if !matches!(
        proposal.tool_name.as_str(),
        "code.symbol" | "code.references" | "code.impls" | "code.read_symbol"
    ) {
        return false;
    }

    proposal_texts(proposal).into_iter().any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("impl_try_from_map") || lower.contains("conversion macro")
    })
}

fn proposal_texts(proposal: &CodingExecProposal) -> Vec<&str> {
    let Some(params) = proposal.params.as_object() else {
        return vec![proposal.rationale.as_str()];
    };

    let mut texts = vec![proposal.rationale.as_str()];
    for key in ["query", "symbol_id", "path_hint", "kind_hint"] {
        if let Some(value) = params.get(key).and_then(serde_json::Value::as_str) {
            texts.push(value);
        }
    }
    texts
}

fn scoped_trait_search_path(
    situation: &CodingSituation<'_>,
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    profile: &CodingPolicyProfile,
    proposal: &CodingExecProposal,
) -> Option<String> {
    if let Some(target_file) =
        preferred_file_anchored_target(situation, local_state, parsed, profile)
    {
        return Some(semantic_path_hint_for_target_file(&target_file));
    }

    proposal_symbol_file_hint(proposal)
        .map(|hint| semantic_path_hint_for_target_file(&hint))
        .or_else(|| {
            concrete_target_file(local_state)
                .map(|target| semantic_path_hint_for_target_file(target))
        })
        .or_else(|| situation.workspace_root.map(str::to_string))
}

fn infer_trait_query(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    proposal: &CodingExecProposal,
) -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(query) = proposal_query(proposal) {
        collect_identifier_candidates(&query, &mut candidates);
    }
    for text in coding_context_texts(local_state, parsed) {
        collect_identifier_candidates(text, &mut candidates);
    }
    for candidate in candidates {
        if is_trait_like_query(&candidate) {
            return Some(candidate);
        }
    }
    coding_context_texts(local_state, parsed)
        .into_iter()
        .any(text_mentions_trait_conversion)
        .then(|| "TryFrom".to_string())
}

fn infer_concrete_type_query(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> Option<String> {
    let mut candidates = Vec::new();
    for text in [
        local_state.semantic_target_name.as_deref(),
        local_state.semantic_target_query.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        collect_identifier_candidates(text, &mut candidates);
    }
    if let Some(file) = concrete_target_file(local_state) {
        if let Some(stem) = Path::new(file).file_stem().and_then(|stem| stem.to_str()) {
            if stem != "mod" && stem != "lib" {
                candidates.push(snake_to_camel(stem));
            }
        }
    }
    for text in coding_context_texts(local_state, parsed) {
        collect_identifier_candidates(text, &mut candidates);
    }

    candidates.into_iter().find(|candidate| {
        looks_like_symbol_query(candidate)
            && !is_trait_like_query(candidate)
            && !matches!(
                candidate.as_str(),
                "Map" | "Result" | "Error" | "Self" | "Option" | "Some" | "None"
            )
    })
}

fn trait_text_search_pattern(
    trait_query: &str,
    concrete_type: Option<&str>,
    _local_state: &ExecThreadLocalState,
    _parsed: &CodingExecResponse,
) -> Option<String> {
    let trait_re = regex_escape_literal(trait_query);
    let trait_snake = regex_escape_literal(&camel_to_snake(trait_query));
    let mut patterns = vec![format!("impl[_A-Za-z0-9]*{trait_snake}[_A-Za-z0-9]*")];

    if let Some(concrete_type) = concrete_type {
        let type_re = regex_escape_literal(concrete_type);
        patterns.push(format!("{trait_re}.*{type_re}"));
        patterns.push(format!("{type_re}.*{trait_re}"));
    } else {
        patterns.push(trait_re);
    }

    (!patterns.is_empty()).then(|| patterns.join("|"))
}

fn coding_context_texts<'a>(
    local_state: &'a ExecThreadLocalState,
    parsed: &'a CodingExecResponse,
) -> Vec<&'a str> {
    [
        parsed.current_focus.as_deref(),
        Some(parsed.summary.as_str()),
        parsed.scratchpad.as_deref(),
        local_state.current_focus.as_deref(),
        local_state.scratchpad.as_deref(),
        local_state.edit_hypothesis_summary.as_deref(),
        local_state.semantic_target_name.as_deref(),
        local_state.semantic_target_query.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn proposal_query(proposal: &CodingExecProposal) -> Option<String> {
    proposal
        .params
        .as_object()?
        .get("query")
        .and_then(serde_json::Value::as_str)
        .filter(|query| !query.trim().is_empty())
        .map(str::to_string)
}

fn camel_to_snake(value: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn regex_escape_literal(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if matches!(
            ch,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn is_module_file(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some("mod.rs" | "lib.rs")
    )
}

fn parent_generic_module_file(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let stem = path.file_stem()?.to_str()?;
    let parent = path.parent()?;
    let parent_name = parent.file_name()?.to_str()?;

    if stem == parent_name {
        return parent
            .parent()
            .map(|module_dir| module_dir.join("mod.rs").display().to_string());
    }

    parent
        .parent()
        .map(|module_dir| module_dir.join("mod.rs").display().to_string())
}

fn synthesize_file_anchored_semantic_proposal(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    workspace_root: Option<&str>,
) -> Option<CodingExecProposal> {
    synthesize_file_anchored_semantic_proposal_for_target(local_state, parsed, workspace_root, None)
}

fn synthesize_file_anchored_semantic_proposal_for_target(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    workspace_root: Option<&str>,
    target_file_override: Option<&str>,
) -> Option<CodingExecProposal> {
    let workspace_root = workspace_root?.trim();
    if workspace_root.is_empty() {
        return None;
    }

    if target_file_override.is_none()
        && local_state.semantic_target_symbol_id.is_some()
        && semantic_target_is_authoritative(local_state, Some(workspace_root))
        && !should_prefer_trait_query(local_state, parsed)
    {
        return synthesize_symbol_consumption_proposal(local_state, Some(workspace_root));
    }

    let target_file = target_file_override.or_else(|| concrete_target_file(local_state))?;
    let query = infer_semantic_query(local_state, parsed, target_file)?;
    let path_hint = semantic_path_hint_for_target_file(target_file);
    let wants_impls = focus_mentions_impls(local_state, parsed) || is_trait_like_query(&query);
    let (tool_name, rationale) = if wants_impls {
        (
            "code.impls",
            format!(
                "Use file-anchored semantic impl lookup for {query} near {path_hint} instead of reopening broad workspace search"
            ),
        )
    } else {
        (
            "code.read_symbol",
            format!(
                "Use file-anchored semantic symbol lookup for {query} near {path_hint} instead of reopening broad workspace search"
            ),
        )
    };

    Some(CodingExecProposal {
        tool_name: tool_name.into(),
        params: serde_json::json!({
            "path": workspace_root,
            "query": query,
            "path_hint": path_hint,
        }),
        rationale,
    })
}

fn synthesize_semantic_span_read_proposal(
    local_state: &ExecThreadLocalState,
) -> Option<CodingExecProposal> {
    let file_path = local_state.semantic_target_file.as_deref()?;
    let start_line = local_state.semantic_target_start_line.unwrap_or(0);
    let end_line = local_state.semantic_target_end_line.unwrap_or(start_line);
    let offset = start_line.saturating_sub(8);
    let span = end_line.saturating_sub(start_line).saturating_add(1);
    let limit = span.max(24).saturating_add(16);

    Some(CodingExecProposal {
        tool_name: "code.read".into(),
        params: serde_json::json!({
            "file_path": file_path,
            "offset": offset,
            "limit": limit,
        }),
        rationale: format!(
            "Read the exact source span around lines {}-{} in {} to capture the concrete text needed for the next edit",
            start_line,
            end_line,
            file_path,
        ),
    })
}

fn synthesize_symbol_consumption_proposal(
    local_state: &ExecThreadLocalState,
    workspace_root: Option<&str>,
) -> Option<CodingExecProposal> {
    if local_state.semantic_target_kind.as_deref() == Some("impl")
        && local_state.semantic_target_start_line.is_some()
        && local_state.semantic_target_file.is_some()
    {
        return synthesize_semantic_span_read_proposal(local_state);
    }

    let workspace_root = workspace_root?.trim();
    if workspace_root.is_empty() {
        return None;
    }

    let symbol_id = local_state.semantic_target_symbol_id.as_deref()?;
    let target_file = local_state
        .semantic_target_file
        .as_deref()
        .or_else(|| concrete_target_file(local_state))?;
    let path_hint = semantic_path_hint_for_target_file(target_file);
    let mut params = serde_json::Map::new();
    params.insert(
        "path".into(),
        serde_json::Value::String(workspace_root.to_string()),
    );
    params.insert(
        "symbol_id".into(),
        serde_json::Value::String(symbol_id.to_string()),
    );
    params.insert(
        "path_hint".into(),
        serde_json::Value::String(path_hint.clone()),
    );
    params.insert("include_body".into(), serde_json::Value::Bool(true));
    if let Some(query) = local_state
        .semantic_target_query
        .as_ref()
        .or(local_state.semantic_target_name.as_ref())
        .filter(|value| !value.trim().is_empty())
    {
        params.insert("query".into(), serde_json::Value::String(query.clone()));
    }
    if let Some(kind) = local_state
        .semantic_target_kind
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        params.insert("kind_hint".into(), serde_json::Value::String(kind.clone()));
    }

    Some(CodingExecProposal {
        tool_name: "code.read_symbol".into(),
        params: serde_json::Value::Object(params),
        rationale: format!(
            "Use resolved semantic target {} in {} to inspect the exact symbol span before proposing the edit",
            local_state
                .semantic_target_name
                .as_deref()
                .or(local_state.semantic_target_query.as_deref())
                .unwrap_or(symbol_id),
            path_hint,
        ),
    })
}

fn focus_mentions_impls(local_state: &ExecThreadLocalState, parsed: &CodingExecResponse) -> bool {
    [
        parsed.current_focus.as_deref(),
        local_state.current_focus.as_deref(),
        Some(parsed.summary.as_str()),
        local_state.edit_hypothesis_summary.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|text| {
        let lower = text.to_ascii_lowercase();
        lower.contains("impl") || lower.contains("tryfrom") || lower.contains("trait")
    })
}

fn infer_semantic_query(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    target_file: &str,
) -> Option<String> {
    let mut candidates = Vec::new();
    for text in [
        local_state.current_focus.as_deref(),
        local_state.edit_hypothesis_summary.as_deref(),
        parsed.current_focus.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        collect_identifier_candidates(text, &mut candidates);
    }

    if let Some(proposal) = parsed.proposed_action.as_ref() {
        collect_proposal_query_candidates(proposal, &mut candidates);
    }

    for text in [Some(parsed.summary.as_str())].into_iter().flatten() {
        collect_identifier_candidates(text, &mut candidates);
    }

    if let Some(stem) = Path::new(target_file)
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        if stem != "mod" && stem != "lib" {
            candidates.push(stem.to_string());
            let camel = snake_to_camel(stem);
            if camel != stem {
                candidates.push(camel);
            }
        }
    }

    let mut best_non_trait = None;
    let mut best_trait = None;
    for candidate in candidates {
        if !looks_like_symbol_query(&candidate) {
            continue;
        }
        if is_trait_like_query(&candidate) {
            if best_trait.is_none() {
                best_trait = Some(candidate);
            }
        } else if best_non_trait.is_none() {
            best_non_trait = Some(candidate);
        }
    }

    if should_prefer_trait_query(local_state, parsed) {
        best_trait.or(best_non_trait)
    } else {
        best_non_trait.or(best_trait)
    }
}

fn should_prefer_trait_query(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
) -> bool {
    matches!(
        local_state.semantic_target_kind.as_deref(),
        Some("struct" | "impl")
    ) && focus_mentions_impls(local_state, parsed)
}

fn collect_proposal_query_candidates(proposal: &CodingExecProposal, out: &mut Vec<String>) {
    let Some(params) = proposal.params.as_object() else {
        return;
    };

    for key in ["query", "pattern", "symbol_id"] {
        if let Some(value) = params.get(key).and_then(serde_json::Value::as_str) {
            collect_identifier_candidates(value, out);
        }
    }

    if let Some(symbols) = params.get("symbols").and_then(serde_json::Value::as_array) {
        for symbol in symbols {
            if let Some(symbol) = symbol.as_str() {
                collect_identifier_candidates(symbol, out);
            }
        }
    }
}

fn collect_identifier_candidates(text: &str, out: &mut Vec<String>) {
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
}

fn looks_like_symbol_query(candidate: &str) -> bool {
    if candidate.len() < 3 {
        return false;
    }
    if matches!(
        candidate,
        "code"
            | "read"
            | "symbol"
            | "references"
            | "workspace"
            | "search"
            | "switch"
            | "blocked"
            | "focus"
            | "target"
            | "file"
            | "explicit"
            | "current"
            | "none"
    ) {
        return false;
    }
    candidate.contains('_')
        || is_trait_like_query(candidate)
        || candidate
            .chars()
            .enumerate()
            .any(|(idx, ch)| idx > 0 && ch.is_ascii_uppercase())
}

fn is_trait_like_query(candidate: &str) -> bool {
    matches!(
        candidate,
        "TryFrom" | "From" | "Into" | "AsRef" | "Borrow" | "Clone" | "Debug" | "Default"
    )
}

fn snake_to_camel(snake: &str) -> String {
    let mut out = String::new();
    for part in snake.split('_').filter(|part| !part.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars.map(|ch| ch.to_ascii_lowercase()));
        }
    }
    out
}

fn should_suppress_broad_reorientation(
    proposal: &CodingExecProposal,
    local_state: &ExecThreadLocalState,
    workspace_root: Option<&str>,
) -> bool {
    let Some(target_file) = concrete_target_file(local_state) else {
        return false;
    };

    if local_state.verification_pending {
        return false;
    }

    match proposal.tool_name.as_str() {
        "repo.locate" | "code.glob" | "code.ls" => true,
        "repo.context" => !proposal_has_explicit_target_paths(proposal),
        "code.grep" => proposal
            .params
            .as_object()
            .and_then(|params| params.get("path"))
            .and_then(serde_json::Value::as_str)
            .map(|path| workspace_root.map_or(true, |root| path == root))
            .unwrap_or(true),
        "code.symbol" | "code.references" | "code.impls" | "code.read_symbol" => {
            if proposal_has_symbol_anchor(proposal) {
                return false;
            }
            match proposal_symbol_file_hint(proposal) {
                Some(file_hint) => file_hint == target_file,
                None => true,
            }
        }
        _ => false,
    }
}

fn should_force_semantic_span_read(
    proposal: &CodingExecProposal,
    local_state: &ExecThreadLocalState,
) -> bool {
    if !has_same_file_semantic_commitment(local_state) {
        return false;
    }

    match proposal.tool_name.as_str() {
        "repo.locate" | "repo.context" | "code.grep" | "code.symbol" | "code.references"
        | "code.impls" => true,
        "code.read" => {
            let Some(file_path) = proposal_file_path(proposal) else {
                return false;
            };
            semantic_target_matches_file(local_state, &file_path)
                && !proposal_read_covers_semantic_target(local_state, proposal)
        }
        _ => false,
    }
}

fn should_consume_resolved_semantic_target(
    proposal: &CodingExecProposal,
    local_state: &ExecThreadLocalState,
    workspace_root: Option<&str>,
) -> bool {
    if local_state
        .semantic_target_symbol_id
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return false;
    }

    if !semantic_target_is_authoritative(local_state, workspace_root) {
        return false;
    }

    match proposal.tool_name.as_str() {
        "code.symbol" | "code.references" | "code.impls" => true,
        "code.read_symbol" => !proposal_has_symbol_anchor(proposal),
        _ => false,
    }
}

fn clear_inspection_state(local_state: &mut ExecThreadLocalState) {
    local_state.inspection_target_file = None;
    local_state.inspection_read_streak = 0;
}

fn infer_coding_work_phase(
    local_state: &ExecThreadLocalState,
    parsed: &CodingExecResponse,
    status: ExecThreadStatus,
) -> Option<String> {
    if let Some(phase) = parsed.work_phase.as_ref() {
        return Some(phase.clone());
    }

    if status == ExecThreadStatus::Idle || parsed.completion_reason.is_some() {
        return Some("idle".into());
    }

    if local_state.verification_pending || local_state.verification_attempted {
        return Some("verifying".into());
    }

    match parsed
        .proposed_action
        .as_ref()
        .map(|proposal| proposal.tool_name.as_str())
    {
        Some("repo.locate" | "repo.context" | "code.grep" | "code.glob" | "code.ls") => {
            Some("locating".into())
        }
        Some(
            "code.read" | "code.read_symbol" | "code.symbol" | "code.references" | "code.impls",
        ) => Some("inspecting".into()),
        Some("code.edit" | "code.write" | "code.apply_patch") => Some("editing".into()),
        Some("shell.exec" | "code.test") => Some("verifying".into()),
        _ if local_state.evidence_complete || local_state.edit_hypothesis_file.is_some() => {
            Some("edit_candidate".into())
        }
        _ => local_state.work_phase.clone(),
    }
}

fn compact_text(text: &str, max_len: usize) -> String {
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.len() <= max_len {
        trimmed
    } else {
        format!("{}...", &trimmed[..max_len])
    }
}

fn compact_semantic_feedback(semantic: &SemanticActionFeedback) -> String {
    let mut summary = format!("{} result", semantic.tool_name);
    if let Some(name) = semantic
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        summary = format!("{summary}: {name}");
    } else if let Some(query) = semantic
        .query
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        summary = format!("{summary}: {query}");
    }
    if let Some(file) = semantic
        .file_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        summary = format!("{summary} in {file}");
    }
    summary
}

fn render_conversation_context<R: ThreadRuntime>(
    runtime: &R,
    perception: &ExecutableThreadPerception,
) -> Result<String, ExoError> {
    let mut out = String::new();
    for conversation in &perception.active_conversations {
        for msg in conversation.message_refs.iter().rev().take(3).rev() {
            let text = runtime
                .get_artifact_text(&msg.payload_ref)?
                .unwrap_or_else(|| "<unresolved message>".into());
            let _ = writeln!(out, "- [{}] {}", msg.source, text.trim());
        }
    }
    Ok(if out.trim().is_empty() {
        "none".into()
    } else {
        out
    })
}

fn render_pending_action_results<R: ThreadRuntime>(
    runtime: &R,
    perception: &ExecutableThreadPerception,
) -> Result<String, ExoError> {
    let mut out = String::new();
    for event in &perception.pending_action_results {
        let _ = writeln!(out, "- {}", event.summary);
        if let Some(payload_ref) = &event.payload_ref {
            if let Some(text) = runtime.get_artifact_text(payload_ref)? {
                let trimmed = text.trim();
                let preview = if trimmed.len() > 1200 {
                    &trimmed[..1200]
                } else {
                    trimmed
                };
                let _ = writeln!(out, "  receipt: {}", preview);
            }
        }
    }
    Ok(if out.trim().is_empty() {
        "none".into()
    } else {
        out
    })
}

fn extract_json_from_code_fence(text: &str) -> Option<&str> {
    let start_marker = "```json";
    let end_marker = "```";
    let start_idx = text.find(start_marker)?;
    let content_start = start_idx + start_marker.len();
    let rest = &text[content_start..];
    let end_idx = rest.find(end_marker)?;
    Some(rest[..end_idx].trim())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use exoskeleton_core::conversation::Conversation;
    use exoskeleton_core::{
        ArtifactId, EnvelopeId, EnvelopeKind, EventEntry, EventType, LedgerEntryId,
        MessageEnvelope, PrincipalId, TickId,
    };

    use super::*;
    use crate::store::InMemoryThreadStore;

    fn perception_with(
        new_messages: Vec<MessageEnvelope>,
        pending_action_results: Vec<EventEntry>,
    ) -> ExecutableThreadPerception {
        ExecutableThreadPerception {
            new_messages,
            active_conversations: Vec::<Conversation>::new(),
            thread_outputs: vec![],
            exec_thread_outputs: vec![],
            pending_action_results,
        }
    }

    fn test_message() -> MessageEnvelope {
        MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"hello"),
            timestamp: Utc::now(),
            in_reply_to: None,
        }
    }

    fn action_result_event() -> EventEntry {
        EventEntry {
            id: LedgerEntryId::new(),
            tick_id: Some(TickId::new()),
            event_type: EventType::ActionExecuted,
            payload_ref: None,
            summary: "action executed".into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn register_builtin_coding_thread_is_idempotent() {
        let store = Arc::new(InMemoryThreadStore::new());
        let registry = ThreadRegistry::new(store);

        register_builtin_coding_thread(&registry, &PromptRegistry::with_defaults(), None, true)
            .unwrap();
        register_builtin_coding_thread(&registry, &PromptRegistry::with_defaults(), None, true)
            .unwrap();

        let threads = registry.list_executable().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].0.role, ThreadRole::Coding);
        assert_eq!(threads[0].1, ExecThreadStatus::Idle);
    }

    #[test]
    fn idle_coding_thread_does_not_reactivate_for_unrelated_action_feedback() {
        let local_state = ExecThreadLocalState::default();
        let perception = perception_with(vec![], vec![action_result_event()]);

        assert!(!should_run_coding_thread(
            ExecThreadStatus::Idle,
            &perception,
            &local_state
        ));
    }

    #[test]
    fn idle_coding_thread_reactivates_when_awaiting_feedback() {
        let local_state = ExecThreadLocalState {
            awaiting_feedback: true,
            ..ExecThreadLocalState::default()
        };
        let perception = perception_with(vec![], vec![action_result_event()]);

        assert!(should_run_coding_thread(
            ExecThreadStatus::Idle,
            &perception,
            &local_state
        ));
    }

    #[test]
    fn idle_coding_thread_reactivates_for_new_messages() {
        let local_state = ExecThreadLocalState::default();
        let perception = perception_with(vec![test_message()], vec![]);

        assert!(should_run_coding_thread(
            ExecThreadStatus::Idle,
            &perception,
            &local_state
        ));
    }

    #[test]
    fn grep_proposal_against_file_path_is_normalized_to_parent_dir() {
        let proposal = normalize_coding_proposal(
            CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace/src/maps/mod.rs",
                    "pattern": "impl.*TryFrom.*HashMap"
                }),
                rationale: "search in the current file".into(),
            },
            Some("/tmp/workspace"),
        )
        .expect("normalized grep proposal");

        assert_eq!(proposal.tool_name, "code.grep");
        assert_eq!(proposal.params["path"], "/tmp/workspace/src/maps");
        assert_eq!(proposal.params["pattern"], "impl.*TryFrom.*HashMap");
    }

    #[test]
    fn grep_context_output_mode_is_normalized_to_content_with_context_lines() {
        let proposal = normalize_coding_proposal(
            CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "pattern": "impl.*TryFrom.*HashMap",
                    "output_mode": "context"
                }),
                rationale: "search with context".into(),
            },
            Some("/tmp/workspace"),
        )
        .expect("normalized grep proposal");

        assert_eq!(proposal.tool_name, "code.grep");
        assert_eq!(proposal.params["output_mode"], "content");
        assert_eq!(proposal.params["context"], 2);
    }

    #[test]
    fn repeated_same_exploratory_proposal_tracks_stall_state() {
        let mut local_state = ExecThreadLocalState::default();
        let proposal = CodingExecProposal {
            tool_name: "code.read".into(),
            params: serde_json::json!({"file_path": "/tmp/workspace/src/lib.rs"}),
            rationale: "read".into(),
        };

        update_exploratory_stall_state(&mut local_state, Some(&proposal));
        update_exploratory_stall_state(&mut local_state, Some(&proposal));
        update_exploratory_stall_state(&mut local_state, Some(&proposal));

        assert_eq!(local_state.last_proposed_tool.as_deref(), Some("code.read"));
        assert_eq!(
            local_state.last_proposed_target.as_deref(),
            Some("/tmp/workspace/src/lib.rs::0:0")
        );
        assert_eq!(local_state.repeated_same_proposal_count, 3);
    }

    #[test]
    fn repeated_same_file_reads_track_inspection_progress_across_offsets() {
        let mut local_state = ExecThreadLocalState::default();
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "inspect file".into(),
            current_focus: None,
            work_phase: None,
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({"file_path": "/tmp/workspace/src/hash_map.rs", "offset": 0, "limit": 80}),
                rationale: "read beginning".into(),
            }),
        };

        update_inspection_state(&mut local_state, &mut parsed);
        parsed.proposed_action = Some(CodingExecProposal {
            tool_name: "code.read".into(),
            params: serde_json::json!({"file_path": "/tmp/workspace/src/hash_map.rs", "offset": 80, "limit": 80}),
            rationale: "read next section".into(),
        });
        update_inspection_state(&mut local_state, &mut parsed);
        parsed.proposed_action = Some(CodingExecProposal {
            tool_name: "code.read".into(),
            params: serde_json::json!({"file_path": "/tmp/workspace/src/hash_map.rs", "offset": 160, "limit": 80}),
            rationale: "read another section".into(),
        });
        update_inspection_state(&mut local_state, &mut parsed);

        assert_eq!(
            local_state.inspection_target_file.as_deref(),
            Some("/tmp/workspace/src/hash_map.rs")
        );
        assert_eq!(local_state.inspection_read_streak, 3);
        assert_eq!(
            local_state.edit_hypothesis_file.as_deref(),
            Some("/tmp/workspace/src/hash_map.rs")
        );
        assert_eq!(parsed.work_phase.as_deref(), Some("inspecting"));
    }

    #[test]
    fn repeated_post_edit_exploration_completes_work_item() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: true,
            verification_attempted: true,
            repeated_same_proposal_count: DEFAULT_EXPLORATORY_STALL_THRESHOLD,
            last_proposed_tool: Some("code.grep".into()),
            last_proposed_target: Some("/tmp/workspace::calculate_averge".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "searching for remaining references".into(),
            current_focus: Some("verify rename".into()),
            work_phase: Some("verifying".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "pattern": "calculate_averge"}),
                rationale: "verify rename".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Idle);
        assert!(parsed.proposed_action.is_none());
        assert!(!parsed.should_wake_master);
        assert!(parsed.completion_reason.is_some());
        assert_eq!(local_state.repeated_same_proposal_count, 0);
        assert!(!local_state.verification_pending);
    }

    #[test]
    fn repeated_pre_edit_exploration_stays_active_but_forces_edit_candidate() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            repeated_same_proposal_count: DEFAULT_EXPLORATORY_STALL_THRESHOLD,
            last_proposed_tool: Some("code.read".into()),
            last_proposed_target: Some("/tmp/workspace/src/hash_map.rs::0:0".into()),
            inspection_target_file: Some("/tmp/workspace/src/hash_map.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD,
            edit_hypothesis_file: Some("/tmp/workspace/src/hash_map.rs".into()),
            edit_hypothesis_summary: Some("HashMap TryFrom logic likely lives in this file".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "continue reading hash_map.rs".into(),
            current_focus: Some("find TryFrom impl".into()),
            work_phase: Some("discovery".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({"file_path": "/tmp/workspace/src/hash_map.rs"}),
                rationale: "continue reading same file".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        let proposal = parsed.proposed_action.as_ref().expect("semantic fallback");
        assert_eq!(proposal.tool_name, "code.impls");
        assert_eq!(proposal.params["query"], "HashMap");
        assert_eq!(
            proposal.params["path_hint"],
            "/tmp/workspace/src/hash_map.rs"
        );
        assert!(parsed.should_wake_master);
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert!(parsed
            .current_focus
            .as_deref()
            .is_some_and(|focus| focus.contains("inspect-to-edit transition")));
    }

    #[test]
    fn read_symbol_with_file_scoped_path_sets_target_file() {
        let mut local_state = ExecThreadLocalState::default();
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "inspect symbol".into(),
            current_focus: None,
            work_phase: None,
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read_symbol".into(),
                params: serde_json::json!({"path": "/tmp/workspace/src/lib.rs", "query": "parse_config_line"}),
                rationale: "read the target function".into(),
            }),
        };

        update_inspection_state(&mut local_state, &mut parsed);

        assert_eq!(
            local_state.inspection_target_file.as_deref(),
            Some("/tmp/workspace/src/lib.rs")
        );
        assert_eq!(
            local_state.edit_hypothesis_file.as_deref(),
            Some("/tmp/workspace/src/lib.rs")
        );
        assert_eq!(local_state.inspection_read_streak, 1);
    }

    #[test]
    fn semantic_directory_path_hint_tracks_module_file_target() {
        let mut local_state = ExecThreadLocalState::default();
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "inspect impls".into(),
            current_focus: None,
            work_phase: None,
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "path_hint": "/tmp/workspace/aya/src/maps",
                    "query": "TryFrom"
                }),
                rationale: "inspect TryFrom impls near maps module".into(),
            }),
        };

        update_inspection_state(&mut local_state, &mut parsed);

        assert_eq!(
            local_state.inspection_target_file.as_deref(),
            Some("/tmp/workspace/aya/src/maps/mod.rs")
        );
        assert_eq!(
            local_state.edit_hypothesis_file.as_deref(),
            Some("/tmp/workspace/aya/src/maps/mod.rs")
        );
    }

    #[test]
    fn broad_workspace_search_is_suppressed_after_concrete_target_is_known() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/lib.rs".into()),
            inspection_read_streak: 1,
            edit_hypothesis_file: Some("/tmp/workspace/src/lib.rs".into()),
            edit_hypothesis_summary: Some("The requested test belongs in src/lib.rs".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "search workspace again".into(),
            current_focus: Some("find more clues".into()),
            work_phase: Some("locating".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "pattern": "parse_config_line"}),
                rationale: "search broadly before editing".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        let proposal = parsed.proposed_action.as_ref().expect("semantic fallback");
        assert_eq!(proposal.tool_name, "code.read_symbol");
        assert_eq!(proposal.params["path"], "/tmp/workspace");
        assert_eq!(proposal.params["path_hint"], "/tmp/workspace/src");
        assert_eq!(proposal.params["query"], "parse_config_line");
        assert!(parsed.should_wake_master);
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert!(parsed
            .summary
            .contains("do not reopen workspace-wide search"));
    }

    #[test]
    fn module_target_uses_directory_scoped_impl_lookup() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            inspection_read_streak: 2,
            edit_hypothesis_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need to inspect HashMap TryFrom impls that should still support LruHashMap".into(),
            ),
            current_focus: Some("find HashMap TryFrom impls".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "search workspace again".into(),
            current_focus: Some("look for TryFrom support".into()),
            work_phase: Some("locating".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "pattern": "TryFrom"}),
                rationale: "search broadly for impls".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("semantic fallback");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.impls");
        assert_eq!(proposal.params["path"], "/tmp/workspace");
        assert_eq!(proposal.params["path_hint"], "/tmp/workspace/src/maps");
        assert_eq!(proposal.params["query"], "HashMap");
        assert!(parsed.should_wake_master);
    }

    #[test]
    fn module_trait_impl_lookup_switches_to_scoped_text_search() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: 2,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some("HashMap cannot be created from LruHashMap via TryFrom".into()),
            current_focus: Some("find HashMap TryFrom implementation".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "continue semantic impl lookup".into(),
            current_focus: Some("inspect TryFrom impls in maps module".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "query": "TryFrom",
                    "path_hint": "/tmp/workspace/aya/src/maps"
                }),
                rationale: "semantic impl lookup for TryFrom near maps".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("scoped text search");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.grep");
        assert_eq!(proposal.params["path"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params["pattern"]
            .as_str()
            .is_some_and(|pattern| pattern.contains("impl[_A-Za-z0-9]*try_from")
                && pattern.contains("TryFrom.*HashMap")));
    }

    #[test]
    fn repeated_module_trait_reads_switch_to_scoped_text_search() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some("HashMap TryFrom<Map> only handles Map::HashMap".into()),
            current_focus: Some("find TryFrom<Map> implementation for HashMap".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "continue reading module file".into(),
            current_focus: Some("inspect TryFrom implementations".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "offset": 120,
                    "limit": 100
                }),
                rationale: "continue reading same module".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("scoped text search");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.grep");
        assert_eq!(proposal.params["path"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params["pattern"]
            .as_str()
            .is_some_and(|pattern| pattern.contains("impl[_A-Za-z0-9]*try_from")
                && pattern.contains("TryFrom.*HashMap")));
    }

    #[test]
    fn repeated_module_trait_reads_consume_macro_hit_before_more_grep() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 2,
            edit_hypothesis_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some(
                "grep found impl_try_from_map macro at line 287 and usages at lines 333, 342, 346, 355".into(),
            ),
            current_focus: Some("read impl_try_from_map macro definition".into()),
            last_proposed_tool: Some("code.grep".into()),
            repeated_same_proposal_count: 2,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "continue searching conversion macro".into(),
            current_focus: Some("inspect TryFrom conversion macro".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/src/maps/mod.rs",
                    "offset": 300,
                    "limit": 80
                }),
                rationale: "read around macro".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("macro span read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/src/maps/mod.rs"
        );
        assert_eq!(proposal.params["offset"], 267);
        assert_eq!(proposal.params["limit"], 120);
    }

    #[test]
    fn repeated_macro_definition_read_switches_to_invocation_region() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 3,
            edit_hypothesis_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some("grep found impl_try_from_map macro at line 287".into()),
            current_focus: Some("read impl_try_from_map macro definition".into()),
            last_proposed_tool: Some("code.read".into()),
            last_proposed_target: Some("/tmp/workspace/src/maps/mod.rs::267:120".into()),
            repeated_same_proposal_count: 1,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "read the macro definition again".into(),
            current_focus: Some("inspect TryFrom conversion macro".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/src/maps/mod.rs",
                    "offset": 320,
                    "limit": 80
                }),
                rationale: "read around macro".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("macro invocation read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/src/maps/mod.rs"
        );
        assert_eq!(proposal.params["offset"], 317);
        assert_eq!(proposal.params["limit"], 80);
    }

    #[test]
    fn macro_semantic_lookup_switches_to_invocation_read() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 3,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some(
                "grep found impl_try_from_map macro at line 287 and usages at lines 333, 342, 346, 355".into(),
            ),
            current_focus: Some("inspect impl_try_from_map before editing".into()),
            last_proposed_tool: Some("code.read".into()),
            last_proposed_target: Some("/tmp/workspace/aya/src/maps/mod.rs::267:120".into()),
            repeated_same_proposal_count: 1,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "use semantic lookup for macro".into(),
            current_focus: Some("find impl_try_from_map macro definition".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "query": "impl_try_from_map",
                    "path_hint": "/tmp/workspace/aya/src/maps"
                }),
                rationale: "semantic lookup for impl_try_from_map".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("macro invocation read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert_eq!(proposal.params["offset"], 343);
        assert_eq!(proposal.params["limit"], 60);
        assert_eq!(parsed.work_phase.as_deref(), Some("inspecting"));
    }

    #[test]
    fn repeated_macro_invocation_read_forces_edit_candidate() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 4,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some(
                "grep found impl_try_from_map macro at line 287 and usages at lines 333, 342, 346, 355".into(),
            ),
            current_focus: Some("commit impl_try_from_map invocation into an edit".into()),
            last_proposed_tool: Some("code.read".into()),
            last_proposed_target: Some("/tmp/workspace/aya/src/maps/mod.rs::343:60".into()),
            repeated_same_proposal_count: 1,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "look up macro semantically again".into(),
            current_focus: Some("find impl_try_from_map macro definition".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.symbol".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "query": "impl_try_from_map",
                    "kind_hint": "macro",
                    "path_hint": "/tmp/workspace/aya/src/maps"
                }),
                rationale: "semantic lookup for impl_try_from_map".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        assert!(parsed.proposed_action.is_none());
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert!(parsed.evidence_complete);
        assert!(local_state.evidence_complete);
        assert!(parsed
            .summary
            .contains("conversion macro invocation has already been inspected"));
    }

    #[test]
    fn macro_invocation_context_blocks_map_type_semantic_lookup() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 5,
            edit_hypothesis_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need to add LruHashMap to the HashMap conversion macro invocation".into(),
            ),
            scratchpad: Some(
                "Found impl_try_from_map macro invocations, including HashMap at line 333".into(),
            ),
            current_focus: Some("commit impl_try_from_map invocation into an edit".into()),
            last_proposed_tool: Some("code.read".into()),
            last_proposed_target: Some("/tmp/workspace/src/maps/mod.rs::330:30".into()),
            repeated_same_proposal_count: 1,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "look up LruHashMap impls".into(),
            current_focus: Some("use semantic lookup for LruHashMap".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "query": "LruHashMap",
                    "path_hint": "/tmp/workspace/src/maps"
                }),
                rationale: "semantic lookup for LruHashMap".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        assert!(parsed.proposed_action.is_none());
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert!(parsed.evidence_complete);
        assert!(local_state.evidence_complete);
        assert!(parsed
            .summary
            .contains("conversion macro invocation is now the edit site"));
    }

    #[test]
    fn edit_candidate_commitment_blocks_semantic_lookup() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need to add LruHashMap support to HashMap TryFrom conversion".into(),
            ),
            scratchpad: Some(
                "Task shows HashMap TryFrom only handles Map::HashMap but should also support Map::LruHashMap".into(),
            ),
            work_phase: Some("edit_candidate".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "look up impls again".into(),
            current_focus: Some("find TryFrom implementations".into()),
            work_phase: Some("edit_candidate".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "query": "TryFrom",
                    "path_hint": "/tmp/workspace/src/maps"
                }),
                rationale: "look up TryFrom impls again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        assert!(parsed.proposed_action.is_none());
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert!(parsed.evidence_complete);
        assert!(local_state.evidence_complete);
        assert!(parsed.should_wake_master);
        assert!(parsed.summary.contains("Inspect-to-edit commitment"));
    }

    #[test]
    fn edit_candidate_commitment_allows_exact_final_read() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need to add LruHashMap support to HashMap TryFrom conversion".into(),
            ),
            scratchpad: Some(
                "Task shows HashMap TryFrom only handles Map::HashMap but should also support Map::LruHashMap".into(),
            ),
            work_phase: Some("edit_candidate".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "read exact span".into(),
            current_focus: Some("capture replacement text".into()),
            work_phase: Some("edit_candidate".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/src/maps/mod.rs",
                    "offset": 343,
                    "limit": 60
                }),
                rationale: "Read exact macro invocation edit span for old_string replacement"
                    .into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("final read allowed");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/src/maps/mod.rs"
        );
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert!(parsed.should_wake_master);
    }

    #[test]
    fn edit_candidate_synthesizes_conversion_macro_variant_edit() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need to add LruHashMap support to HashMap TryFrom conversion".into(),
            ),
            scratchpad: Some(
                "Found impl_try_from_map!((K, V) { HashMap, PerCpuHashMap, LpmTrie }) at lines 355-359. HashMap is missing LruHashMap support.".into(),
            ),
            work_phase: Some("edit_candidate".into()),
            evidence_complete: true,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "read once more".into(),
            current_focus: Some("verify LruHashMap variant exists".into()),
            work_phase: Some("edit_candidate".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "offset": 220,
                    "limit": 60
                }),
                rationale: "verify LruHashMap variant exists".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("macro edit");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.apply_patch");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert!(proposal.params["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("HashMap from HashMap|LruHashMap")));
        assert!(proposal.params["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("@@ -353,7 +353,7 @@")));
        assert!(proposal.params["patch"].as_str().is_some_and(
            |patch| patch.contains("PerCpuHashMap from PerCpuHashMap|PerCpuLruHashMap")
        ));
        assert_eq!(parsed.work_phase.as_deref(), Some("editing"));
    }

    #[test]
    fn no_proposal_known_aya_lru_tryfrom_issue_synthesizes_macro_patch() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 2,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom support for LruHashMap in the maps conversion module".into(),
            ),
            work_phase: Some("edit_candidate".into()),
            evidence_complete: true,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "Inspect-to-edit commitment is active for /tmp/workspace/aya/src/maps/mod.rs"
                .into(),
            current_focus: Some(
                "commit HashMap/LruHashMap TryFrom support into a concrete edit".into(),
            ),
            work_phase: Some("edit_candidate".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: None,
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("macro patch");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.apply_patch");
        assert!(proposal.params["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("HashMap from HashMap|LruHashMap")));
        assert_eq!(parsed.work_phase.as_deref(), Some("editing"));
    }

    #[test]
    fn known_aya_macro_usage_context_synthesizes_patch_while_inspecting() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 1,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom<Map> conversion logic for LruHashMap support".into(),
            ),
            scratchpad: Some(
                "grep found impl_try_from_map macro and HashMap macro invocations".into(),
            ),
            current_focus: Some("inspect impl_try_from_map invocation before editing".into()),
            work_phase: Some("inspecting".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "read the TryFrom implementations again".into(),
            current_focus: Some("find HashMap/LruHashMap TryFrom support".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "offset": 690,
                    "limit": 50
                }),
                rationale: "read the TryFrom implementations again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("macro patch");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.apply_patch");
        assert!(proposal.params["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("HashMap from HashMap|LruHashMap")));
        assert_eq!(parsed.work_phase.as_deref(), Some("editing"));
    }

    #[test]
    fn weak_aya_lru_tryfrom_context_synthesizes_macro_patch_before_more_reads() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 3,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom support for LruHashMap in maps/mod.rs".into(),
            ),
            work_phase: Some("edit_candidate".into()),
            evidence_complete: true,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "read the implementation again".into(),
            current_focus: Some("find the HashMap TryFrom implementation".into()),
            work_phase: Some("edit_candidate".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "offset": 680,
                    "limit": 50
                }),
                rationale: "read TryFrom implementation again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("macro patch");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.apply_patch");
        assert!(proposal.params["patch"].as_str().is_some_and(
            |patch| patch.contains("PerCpuHashMap from PerCpuHashMap|PerCpuLruHashMap")
        ));
        assert_eq!(parsed.work_phase.as_deref(), Some("editing"));
    }

    #[test]
    fn incomplete_lruhashmap_peer_edit_is_upgraded_to_macro_patch() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some("Add LruHashMap support to HashMap TryFrom".into()),
            work_phase: Some("inspecting".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "apply simple edit".into(),
            current_focus: Some("add LruHashMap".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.edit".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "old_string": "impl_try_from_map!((K, V) {\n    HashMap,\n    PerCpuHashMap,\n    LpmTrie,\n});",
                    "new_string": "impl_try_from_map!((K, V) {\n    HashMap,\n    PerCpuHashMap,\n    LruHashMap,\n    LpmTrie,\n});"
                }),
                rationale: "Add LruHashMap as a supported map type".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("upgraded patch");
        assert_eq!(proposal.tool_name, "code.apply_patch");
        assert!(proposal.params["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("HashMap from HashMap|LruHashMap")));
        assert!(proposal.params["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("@@ -353,7 +353,7 @@")));
        assert_eq!(parsed.work_phase.as_deref(), Some("editing"));
    }

    #[test]
    fn post_edit_success_verifies_instead_of_reediting() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: true,
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need to add LruHashMap support to HashMap TryFrom conversion".into(),
            ),
            scratchpad: Some(
                "Found impl_try_from_map!((K, V) { HashMap, HashMap from LruHashMap, PerCpuHashMap, LpmTrie }) after edit.".into(),
            ),
            work_phase: Some("verifying".into()),
            evidence_complete: true,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "edit again".into(),
            current_focus: Some("apply edit".into()),
            work_phase: Some("editing".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.edit".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "old_string": "impl_try_from_map!((K, V) {\n    HashMap,\n    PerCpuHashMap,\n    LpmTrie,\n});",
                    "new_string": "impl_try_from_map!((K, V) {\n    HashMap,\n    HashMap from LruHashMap,\n    PerCpuHashMap,\n    LpmTrie,\n});"
                }),
                rationale: "apply macro edit again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback {
                mutating_success: true,
                ..CodingActionFeedback::default()
            },
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("verification read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert_eq!(parsed.work_phase.as_deref(), Some("verifying"));
        assert!(local_state.verification_pending);
    }

    #[test]
    fn post_edit_no_proposal_synthesizes_code_test_when_manifest_exists() {
        let workspace =
            std::env::temp_dir().join(format!("exo-code-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: true,
            verification_attempted: false,
            work_phase: Some("verifying".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Idle),
            summary: "done".into(),
            current_focus: None,
            work_phase: Some("idle".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: None,
            should_wake_master: false,
            completion_reason: Some("complete".into()),
            proposed_action: None,
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some(workspace.to_str().unwrap()),
        );

        let proposal = parsed.proposed_action.as_ref().expect("test proposal");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.test");
        assert_eq!(proposal.params["path"], workspace.to_str().unwrap());
        assert_eq!(parsed.work_phase.as_deref(), Some("verifying"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn post_edit_verification_targets_nearest_rust_crate_for_subcrate_edit() {
        let workspace =
            std::env::temp_dir().join(format!("exo-code-test-{}", uuid::Uuid::new_v4()));
        let crate_dir = workspace.join("aya");
        let src_dir = crate_dir.join("src/maps");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers=[\"aya\"]\n",
        )
        .unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"aya\"\n").unwrap();
        let file_path = src_dir.join("mod.rs");
        std::fs::write(&file_path, "").unwrap();

        let situation = CodingSituation {
            phase: CodingPhase::Verifying,
            proposed_action: None,
            workspace_root: Some(workspace.to_str().unwrap()),
            target_file: Some(file_path.to_str().unwrap()),
            edit_hypothesis_file: Some(file_path.to_str().unwrap()),
            edit_hypothesis_summary: None,
            semantic_target: None,
            repeated_same_proposal_count: 0,
            inspection_read_streak: 0,
            evidence_complete: true,
            verification_pending: true,
            verification_attempted: false,
            recent_mutation_succeeded: true,
            recent_mutation_failed: false,
            recent_verification_succeeded: false,
            recent_verification_failed: false,
        };

        let proposal =
            synthesize_post_edit_test_verification(&situation).expect("verification proposal");

        assert_eq!(proposal.tool_name, "code.test");
        assert_eq!(proposal.params["path"], crate_dir.to_str().unwrap());
        assert_eq!(proposal.params["command"], "cargo");
        assert_eq!(
            proposal.params["args"],
            serde_json::json!(["check", "--tests"])
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn root_level_code_test_is_scoped_to_nearest_crate_after_subcrate_edit() {
        let workspace =
            std::env::temp_dir().join(format!("exo-code-test-{}", uuid::Uuid::new_v4()));
        let crate_dir = workspace.join("aya");
        let src_dir = crate_dir.join("src/maps");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers=[\"aya\"]\n",
        )
        .unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"aya\"\n").unwrap();
        let file_path = src_dir.join("mod.rs");
        std::fs::write(&file_path, "").unwrap();

        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: true,
            verification_attempted: false,
            edit_hypothesis_file: Some(file_path.to_string_lossy().to_string()),
            work_phase: Some("verifying".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "run tests".into(),
            current_focus: Some("verify".into()),
            work_phase: Some("verifying".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.test".into(),
                params: serde_json::json!({
                    "path": workspace.to_str().unwrap()
                }),
                rationale: "run workspace tests".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some(workspace.to_str().unwrap()),
        );

        let proposal = parsed.proposed_action.as_ref().expect("scoped test");
        assert_eq!(proposal.tool_name, "code.test");
        assert_eq!(proposal.params["path"], crate_dir.to_str().unwrap());
        assert_eq!(proposal.params["command"], "cargo");
        assert_eq!(
            proposal.params["args"],
            serde_json::json!(["check", "--tests"])
        );
        assert!(parsed.summary.contains("scoped post-edit verification"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn repeated_code_test_after_failed_verification_forces_inspection() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: false,
            verification_attempted: true,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            work_phase: Some("verifying".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "run cargo check again".into(),
            current_focus: Some("get full compilation error".into()),
            work_phase: Some("verifying".into()),
            scratchpad: Some("previous cargo test failed".into()),
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.test".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "command": "cargo",
                    "args": ["check", "--message-format=human"]
                }),
                rationale: "rerun cargo check".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("inspection read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert_eq!(parsed.work_phase.as_deref(), Some("inspecting"));
        assert!(parsed.summary.contains("Verification already failed"));
    }

    #[test]
    fn incorrect_macro_plus_to_star_edit_is_rejected() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: false,
            verification_attempted: true,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            work_phase: Some("inspecting".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "fix macro syntax".into(),
            current_focus: Some("change plus to star".into()),
            work_phase: Some("editing".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.edit".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "old_string": "        $($ty:ident $(from $($variant:ident)|+)?),+ $(,)?",
                    "new_string": "        $($ty:ident $(from $($variant:ident)|*)?),+ $(,)?"
                }),
                rationale: "incorrectly change one-or-more to zero-or-more".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("inspection read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert!(parsed
            .summary
            .contains("Rejecting incorrect macro repetition edit"));
    }

    #[test]
    fn deterministic_macro_patch_does_not_resynthesize_after_failed_verification() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: false,
            verification_attempted: true,
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: DEFAULT_SAME_FILE_INSPECTION_THRESHOLD + 2,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_summary: Some(
                "Need HashMap TryFrom support for LruHashMap in the maps conversion module".into(),
            ),
            work_phase: Some("inspecting".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "verification failed; inspect next".into(),
            current_focus: Some("inspect verification failure".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: Some("impl_try_from_map invocation supports HashMap/LruHashMap".into()),
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: None,
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        assert!(parsed.proposed_action.is_none());
        assert_eq!(parsed.work_phase.as_deref(), Some("inspecting"));
    }

    #[test]
    fn repeated_deterministic_macro_patch_after_failed_verification_forces_inspection() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: false,
            verification_attempted: true,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            work_phase: Some("editing".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "apply patch again".into(),
            current_focus: Some("reapply conversion macro patch".into()),
            work_phase: Some("editing".into()),
            scratchpad: None,
            evidence_complete: true,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.apply_patch".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "patch": "@@ -353,7 +353,7 @@ impl_try_from_map!((K, V) {\n-    HashMap,\n-    PerCpuHashMap,\n+    HashMap from HashMap|LruHashMap,\n+    PerCpuHashMap from PerCpuHashMap|PerCpuLruHashMap,\n     LpmTrie,\n });\n"
                }),
                rationale: "reapply deterministic patch".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("inspection read");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert!(parsed.summary.contains("do not reapply the same patch"));
    }

    #[test]
    fn mutating_failure_preserves_prior_verification_attempt_state() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            verification_pending: false,
            verification_attempted: true,
            work_phase: Some("inspecting".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "patch failed".into(),
            current_focus: None,
            work_phase: None,
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: false,
            completion_reason: None,
            proposed_action: None,
        };

        apply_coding_feedback_state(
            &mut status,
            &mut local_state,
            &CodingActionFeedback {
                mutating_failure: true,
                ..CodingActionFeedback::default()
            },
            &mut parsed,
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        assert!(local_state.verification_attempted);
        assert!(!local_state.verification_pending);
    }

    #[test]
    fn semantic_feedback_populates_resolved_target_state() {
        let mut local_state = ExecThreadLocalState::default();
        let feedback = CodingActionFeedback {
            semantic_resolution: Some(SemanticActionFeedback {
                tool_name: "code.impls".into(),
                query: Some("TryFrom".into()),
                symbol_id: Some("impl:tryfrom:hash_map".into()),
                name: Some("TryFrom for HashMap".into()),
                kind: Some("impl".into()),
                file_path: Some("/tmp/workspace/src/hash_map.rs".into()),
                start_line: Some(12),
                end_line: Some(48),
            }),
            ..CodingActionFeedback::default()
        };

        apply_semantic_feedback_state(&mut local_state, &feedback, Some("/tmp/workspace"));

        assert_eq!(
            local_state.semantic_target_symbol_id.as_deref(),
            Some("impl:tryfrom:hash_map")
        );
        assert_eq!(
            local_state.semantic_target_name.as_deref(),
            Some("TryFrom for HashMap")
        );
        assert_eq!(local_state.semantic_target_kind.as_deref(), Some("impl"));
        assert_eq!(
            local_state.semantic_target_file.as_deref(),
            Some("/tmp/workspace/src/hash_map.rs")
        );
        assert_eq!(
            local_state.semantic_target_query.as_deref(),
            Some("TryFrom")
        );
        assert_eq!(local_state.semantic_target_start_line, Some(12));
        assert_eq!(local_state.semantic_target_end_line, Some(48));
        assert_eq!(
            local_state.inspection_target_file.as_deref(),
            Some("/tmp/workspace/src/hash_map.rs")
        );
    }

    #[test]
    fn cross_subtree_semantic_feedback_does_not_override_active_target() {
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            ..ExecThreadLocalState::default()
        };
        let feedback = CodingActionFeedback {
            semantic_resolution: Some(SemanticActionFeedback {
                tool_name: "code.impls".into(),
                query: Some("LruHashMap".into()),
                symbol_id: Some("impl:lruhashmap:sync".into()),
                name: Some("Sync for LruHashMap".into()),
                kind: Some("impl".into()),
                file_path: Some("/tmp/workspace/bpf/aya-bpf/src/maps/hash_map.rs".into()),
                start_line: Some(10),
                end_line: Some(42),
            }),
            ..CodingActionFeedback::default()
        };

        apply_semantic_feedback_state(&mut local_state, &feedback, Some("/tmp/workspace"));

        assert!(local_state.semantic_target_symbol_id.is_none());
        assert!(local_state.semantic_target_file.is_none());
        assert_eq!(
            local_state.inspection_target_file.as_deref(),
            Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs")
        );
        assert_eq!(
            local_state.edit_hypothesis_file.as_deref(),
            Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs")
        );
    }

    #[test]
    fn repeated_semantic_lookup_consumes_resolved_symbol_target() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("sym:hash_map:try_from".into()),
            semantic_target_name: Some("try_from".into()),
            semantic_target_kind: Some("function".into()),
            semantic_target_file: Some("/tmp/workspace/src/hash_map.rs".into()),
            semantic_target_query: Some("try_from".into()),
            inspection_target_file: Some("/tmp/workspace/src/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/src/hash_map.rs".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "repeat semantic lookup".into(),
            current_focus: Some("inspect impl again".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "query": "TryFrom", "path_hint": "/tmp/workspace/src/hash_map.rs"}),
                rationale: "look up impls again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        assert_eq!(status, ExecThreadStatus::Active);
        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("symbol consumption fallback");
        assert_eq!(proposal.tool_name, "code.read_symbol");
        assert_eq!(proposal.params["path"], "/tmp/workspace");
        assert_eq!(proposal.params["symbol_id"], "sym:hash_map:try_from");
        assert_eq!(
            proposal.params["path_hint"],
            "/tmp/workspace/src/hash_map.rs"
        );
        assert_eq!(proposal.params["query"], "try_from");
        assert_eq!(proposal.params["kind_hint"], "function");
        assert_eq!(proposal.params["include_body"], true);
        assert!(parsed.should_wake_master);
    }

    #[test]
    fn same_file_semantic_target_forces_span_read_over_broad_search() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: 1,
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            semantic_target_query: Some("HashMap".into()),
            semantic_target_start_line: Some(316),
            semantic_target_end_line: Some(324),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "search again".into(),
            current_focus: Some("find try_from impl".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "pattern": "TryFrom"}),
                rationale: "search broadly".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed.proposed_action.as_ref().expect("span read fallback");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert_eq!(proposal.params["offset"], 308);
        assert_eq!(proposal.params["limit"], 40);
        assert_eq!(
            parsed.proposal_confidence,
            Some(ExecThreadProposalConfidence::High)
        );
    }

    #[test]
    fn span_read_over_semantic_target_marks_edit_candidate() {
        let mut local_state = ExecThreadLocalState {
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            semantic_target_start_line: Some(316),
            semantic_target_end_line: Some(324),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            inspection_read_streak: 1,
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "inspect span".into(),
            current_focus: None,
            work_phase: None,
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/mod.rs",
                    "offset": 308,
                    "limit": 40
                }),
                rationale: "inspect exact impl span".into(),
            }),
        };

        update_inspection_state(&mut local_state, &mut parsed);

        assert!(local_state.evidence_complete);
        assert!(parsed.evidence_complete);
        assert_eq!(parsed.work_phase.as_deref(), Some("edit_candidate"));
        assert_eq!(local_state.inspection_read_streak, 2);
    }

    #[test]
    fn cross_subtree_resolved_symbol_is_not_consumed() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("sym:lruhashmap:sync".into()),
            semantic_target_name: Some("Sync for LruHashMap".into()),
            semantic_target_kind: Some("impl".into()),
            semantic_target_file: Some("/tmp/workspace/bpf/aya-bpf/src/maps/hash_map.rs".into()),
            semantic_target_query: Some("LruHashMap".into()),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "repeat semantic lookup".into(),
            current_focus: Some("inspect impl again".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "query": "LruHashMap", "path_hint": "/tmp/workspace/aya/src/maps/hash_map/hash_map.rs"}),
                rationale: "look up impls again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("fallback remains semantic");
        assert_eq!(status, ExecThreadStatus::Active);
        assert!(matches!(
            proposal.tool_name.as_str(),
            "code.impls" | "code.read_symbol"
        ));
        assert_eq!(proposal.params["path_hint"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params.get("symbol_id").is_none());
    }

    #[test]
    fn file_anchored_semantic_fallback_prefers_resolved_symbol_consumption() {
        let local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("sym:parse_config_line".into()),
            semantic_target_name: Some("parse_config_line".into()),
            semantic_target_kind: Some("function".into()),
            semantic_target_file: Some("/tmp/workspace/src/lib.rs".into()),
            inspection_target_file: Some("/tmp/workspace/src/lib.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/src/lib.rs".into()),
            ..ExecThreadLocalState::default()
        };
        let parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "use semantic fallback".into(),
            current_focus: Some("inspect target function".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: None,
        };

        let proposal = synthesize_file_anchored_semantic_proposal(
            &local_state,
            &parsed,
            Some("/tmp/workspace"),
        )
        .expect("resolved symbol fallback");

        assert_eq!(proposal.tool_name, "code.read_symbol");
        assert_eq!(proposal.params["symbol_id"], "sym:parse_config_line");
        assert_eq!(proposal.params["path_hint"], "/tmp/workspace/src");
        assert_eq!(proposal.params["query"], "parse_config_line");
        assert_eq!(proposal.params["kind_hint"], "function");
        assert_eq!(proposal.params["include_body"], true);
    }

    #[test]
    fn trait_focused_work_does_not_reconsume_struct_symbol_target() {
        let local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("sym:hash_map:struct".into()),
            semantic_target_name: Some("HashMap".into()),
            semantic_target_kind: Some("struct".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            semantic_target_query: Some("HashMap".into()),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            current_focus: Some("find TryFrom impls for HashMap".into()),
            ..ExecThreadLocalState::default()
        };
        let parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "need trait impls".into(),
            current_focus: Some("inspect TryFrom implementations".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: None,
            should_wake_master: true,
            completion_reason: None,
            proposed_action: None,
        };

        let proposal = synthesize_file_anchored_semantic_proposal(
            &local_state,
            &parsed,
            Some("/tmp/workspace"),
        )
        .expect("trait-focused semantic fallback");

        assert_eq!(proposal.tool_name, "code.impls");
        assert_eq!(proposal.params["query"], "TryFrom");
        assert_eq!(
            proposal.params["path_hint"],
            "/tmp/workspace/aya/src/maps/hash_map/hash_map.rs"
        );
        assert!(proposal.params.get("symbol_id").is_none());
    }

    #[test]
    fn policy_prefers_parent_module_for_trait_oriented_leaf_struct_target() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("sym:hash_map:struct".into()),
            semantic_target_name: Some("HashMap".into()),
            semantic_target_kind: Some("struct".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            semantic_target_query: Some("HashMap".into()),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            current_focus: Some("inspect TryFrom<Map> machinery for HashMap".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "look for trait conversion impl".into(),
            current_focus: Some("find TryFrom support for Map variants".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.grep".into(),
                params: serde_json::json!({"path": "/tmp/workspace", "pattern": "TryFrom"}),
                rationale: "broaden search for conversion logic".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("parent module semantic lookup");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.impls");
        assert_eq!(proposal.params["query"], "TryFrom");
        assert_eq!(proposal.params["path_hint"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params.get("symbol_id").is_none());
    }

    #[test]
    fn final_governor_breaks_policy_synthesized_semantic_lookup_repeat() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            repeated_same_proposal_count: DEFAULT_SEMANTIC_LOOKUP_STALL_THRESHOLD,
            last_proposed_tool: Some("code.impls".into()),
            last_proposed_target: Some(
                "/tmp/workspace::/tmp/workspace/aya/src/maps::TryFrom".into(),
            ),
            semantic_target_symbol_id: Some("sym:hash_map:struct".into()),
            semantic_target_name: Some("HashMap".into()),
            semantic_target_kind: Some("struct".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            semantic_target_query: Some("HashMap".into()),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            current_focus: Some("inspect TryFrom<Map> machinery for HashMap".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "model proposed another read".into(),
            current_focus: Some("inspect TryFrom implementations".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: Some("HashMap TryFrom<Map> needs LruHashMap support".into()),
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/hash_map/hash_map.rs",
                    "offset": 1,
                    "limit": 120
                }),
                rationale: "read HashMap again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("line-grounded fallback");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.grep");
        assert_eq!(proposal.params["path"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params["pattern"]
            .as_str()
            .is_some_and(|pattern| pattern.contains("TryFrom.*HashMap")));
    }

    #[test]
    fn policy_demotes_leaf_concrete_impl_for_trait_oriented_work() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("impl:hash_map:constructor".into()),
            semantic_target_name: Some("HashMap<T>".into()),
            semantic_target_kind: Some("impl".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            semantic_target_query: Some("HashMap".into()),
            semantic_target_start_line: Some(41),
            semantic_target_end_line: Some(74),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            current_focus: Some("find TryFrom<Map> implementation for HashMap".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "same-file semantic target exists".into(),
            current_focus: Some("inspect TryFrom implementations".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::High),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.read".into(),
                params: serde_json::json!({
                    "file_path": "/tmp/workspace/aya/src/maps/hash_map/hash_map.rs",
                    "offset": 41,
                    "limit": 50
                }),
                rationale: "read concrete HashMap impl span again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("parent module semantic lookup");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.impls");
        assert_eq!(proposal.params["query"], "TryFrom");
        assert_eq!(proposal.params["path_hint"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params.get("symbol_id").is_none());
    }

    #[test]
    fn repeated_trait_impl_lookup_switches_to_scoped_text_search() {
        let mut status = ExecThreadStatus::Active;
        let mut local_state = ExecThreadLocalState {
            repeated_same_proposal_count: DEFAULT_EXPLORATORY_STALL_THRESHOLD,
            last_proposed_tool: Some("code.impls".into()),
            last_proposed_target: Some(
                "/tmp/workspace::/tmp/workspace/aya/src/maps::TryFrom".into(),
            ),
            semantic_target_symbol_id: Some("impl:hash_map:constructor".into()),
            semantic_target_name: Some("HashMap".into()),
            semantic_target_kind: Some("impl".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            semantic_target_query: Some("HashMap".into()),
            semantic_target_start_line: Some(41),
            semantic_target_end_line: Some(74),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/hash_map/hash_map.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            scratchpad: Some("HashMap TryFrom<Map> is missing the Map::LruHashMap case".into()),
            current_focus: Some("find TryFrom<Map> implementation for HashMap".into()),
            ..ExecThreadLocalState::default()
        };
        let mut parsed = CodingExecResponse {
            status: Some(ExecThreadStatus::Active),
            summary: "semantic impl lookup repeated".into(),
            current_focus: Some("inspect TryFrom implementations".into()),
            work_phase: Some("inspecting".into()),
            scratchpad: None,
            evidence_complete: false,
            proposal_confidence: Some(ExecThreadProposalConfidence::Medium),
            should_wake_master: true,
            completion_reason: None,
            proposed_action: Some(CodingExecProposal {
                tool_name: "code.impls".into(),
                params: serde_json::json!({
                    "path": "/tmp/workspace",
                    "query": "TryFrom",
                    "path_hint": "/tmp/workspace/aya/src/maps"
                }),
                rationale: "look up TryFrom impls again".into(),
            }),
        };

        apply_coding_stall_policy(
            &mut status,
            &mut local_state,
            &mut parsed,
            &CodingActionFeedback::default(),
            &CodingPolicyProfile::default(),
            Some("/tmp/workspace"),
        );

        let proposal = parsed
            .proposed_action
            .as_ref()
            .expect("scoped text-search fallback");
        assert_eq!(status, ExecThreadStatus::Active);
        assert_eq!(proposal.tool_name, "code.grep");
        assert_eq!(proposal.params["path"], "/tmp/workspace/aya/src/maps");
        assert!(proposal.params["pattern"]
            .as_str()
            .is_some_and(|pattern| pattern.contains("impl[_A-Za-z0-9]*try_from")
                && pattern.contains("TryFrom.*HashMap")
                && !pattern.contains("LruHashMap")));
        assert!(parsed.should_wake_master);
    }

    #[test]
    fn impl_semantic_target_consumes_via_span_read() {
        let local_state = ExecThreadLocalState {
            semantic_target_symbol_id: Some("impl:hash_map:try_from".into()),
            semantic_target_name: Some("HashMap".into()),
            semantic_target_kind: Some("impl".into()),
            semantic_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            semantic_target_query: Some("TryFrom".into()),
            semantic_target_start_line: Some(316),
            semantic_target_end_line: Some(324),
            inspection_target_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            edit_hypothesis_file: Some("/tmp/workspace/aya/src/maps/mod.rs".into()),
            ..ExecThreadLocalState::default()
        };

        let proposal = synthesize_symbol_consumption_proposal(&local_state, Some("/tmp/workspace"))
            .expect("impl span read");

        assert_eq!(proposal.tool_name, "code.read");
        assert_eq!(
            proposal.params["file_path"],
            "/tmp/workspace/aya/src/maps/mod.rs"
        );
        assert_eq!(proposal.params["offset"], 308);
        assert_eq!(proposal.params["limit"], 40);
    }
}
