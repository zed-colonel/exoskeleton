//! Executable thread execution within the master loop.

use std::fmt::Write as _;

use actionqueue_executor_local::CancellationToken;
use exoskeleton_core::llm::{LlmMessage, LlmRequest, LlmRole};
use exoskeleton_core::tick::ExecThreadContribution;
use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, EventType, ExecThreadKind, ExecThreadLocalState,
    ExecThreadOutput, ExecThreadProposal, ExecThreadProposalConfidence, ExecThreadSpec,
    ExecThreadStatus, ExoError, StateSnapshot, TickId,
};
use serde::{Deserialize, Serialize};

use super::types::extract_json_from_code_fence;
use super::{KernelContext, PerceptionResult};
use crate::cognitive_engine::CognitiveHandler;

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

pub fn execute_due_exec_threads(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    snapshot: &StateSnapshot,
    perception: &PerceptionResult,
    tick_id: TickId,
    cancellation: &CancellationToken,
) -> Result<Vec<ExecThreadContribution>, ExoError> {
    let mut contributions = Vec::new();
    for (spec, status) in kernel.exec_thread_registry.list()? {
        if cancellation.is_cancelled() {
            break;
        }
        let local_state = kernel.exec_thread_registry.local_state(spec.thread_id)?;
        if !should_run_exec_thread(&spec, status, perception, &local_state) {
            continue;
        }
        let output = execute_exec_thread(
            handler,
            kernel,
            &spec,
            snapshot,
            perception,
            tick_id,
            cancellation,
            local_state,
        )?;
        contributions.push(ExecThreadContribution {
            thread_id: output.thread_id,
            kind: output.kind,
            artifact_id: output.artifact_id.clone(),
            summary: output.summary.clone(),
            proposal_id: output
                .proposed_action
                .as_ref()
                .map(|p| p.proposal_id.clone()),
            proposed_action_summary: output
                .proposed_action
                .as_ref()
                .map(|p| format!("{}: {}", p.tool_name, p.rationale)),
        });
        kernel
            .exec_thread_registry
            .update_status(output.thread_id, output.status)?;
        kernel
            .exec_thread_registry
            .save_local_state(output.thread_id, &output.local_state)?;
        kernel.exec_thread_registry.save_output(&output)?;
    }
    Ok(contributions)
}

fn execute_exec_thread(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    spec: &ExecThreadSpec,
    snapshot: &StateSnapshot,
    perception: &PerceptionResult,
    tick_id: TickId,
    cancellation: &CancellationToken,
    local_state: ExecThreadLocalState,
) -> Result<ExecThreadOutput, ExoError> {
    match spec.kind {
        ExecThreadKind::Coding => execute_coding_thread(
            handler,
            kernel,
            spec,
            snapshot,
            perception,
            tick_id,
            cancellation,
            local_state,
        ),
    }
}

fn execute_coding_thread(
    handler: &CognitiveHandler,
    kernel: &KernelContext,
    spec: &ExecThreadSpec,
    snapshot: &StateSnapshot,
    perception: &PerceptionResult,
    tick_id: TickId,
    cancellation: &CancellationToken,
    local_state: ExecThreadLocalState,
) -> Result<ExecThreadOutput, ExoError> {
    let prompt = build_coding_exec_prompt(spec, kernel, snapshot, perception, &local_state);
    let request = LlmRequest {
        backend: Some(handler.default_backend),
        system_prompt: Some(
            kernel
                .prompt_registry
                .get("coding-thread-system")
                .unwrap_or("You are an executable coding thread. Maintain local work state and propose at most one next external action as JSON.")
                .to_string(),
        ),
        messages: vec![LlmMessage::text(LlmRole::User, prompt)],
        max_output_tokens: kernel.max_output_tokens / 2,
        temperature: Some(0.3),
        stop_sequences: vec![],
        stream: false,
        tools: vec![],
    };
    let result =
        crate::llm::direct::handler_direct_llm_call(handler, kernel, &request, cancellation)?;
    let response_text = result.response.text();
    let mut parsed = parse_exec_response(&response_text);
    parsed.proposed_action = parsed.proposed_action.and_then(normalize_coding_proposal);
    let feedback = coding_feedback_from_latest_tick(kernel, spec.thread_id);
    let mut status = canonicalize_exec_thread_status(
        parsed
            .status
            .unwrap_or_else(|| default_coding_status(&parsed, perception, &local_state)),
    );
    if parsed.proposal_confidence.is_none() && parsed.proposed_action.is_some() {
        parsed.proposal_confidence = Some(if parsed.evidence_complete {
            ExecThreadProposalConfidence::High
        } else {
            ExecThreadProposalConfidence::Medium
        });
    }

    let mut next_local_state = ExecThreadLocalState {
        scratchpad: parsed.scratchpad.clone(),
        current_focus: parsed.current_focus.clone(),
        last_completion_reason: parsed
            .completion_reason
            .clone()
            .or(local_state.last_completion_reason.clone()),
        work_phase: parsed.work_phase.clone().or(local_state.work_phase.clone()),
        evidence_complete: parsed.evidence_complete,
        verification_pending: local_state.verification_pending,
        verification_attempted: local_state.verification_attempted,
        proposal_confidence: parsed
            .proposal_confidence
            .or(local_state.proposal_confidence),
        awaiting_feedback: parsed.proposed_action.is_some(),
        should_wake_master: parsed.should_wake_master,
    };
    apply_coding_feedback_state(&mut status, &mut next_local_state, &feedback, &mut parsed);
    let output = ExecThreadOutput {
        thread_id: spec.thread_id,
        tick_id,
        artifact_id: ArtifactId::from_content(response_text.as_bytes()),
        kind: ExecThreadKind::Coding,
        summary: parsed.summary,
        status,
        evidence_complete: next_local_state.evidence_complete,
        proposal_confidence: next_local_state.proposal_confidence,
        proposed_action: parsed.proposed_action.map(|proposal| ExecThreadProposal {
            proposal_id: format!("{}-{}", tick_id, spec.thread_id),
            tool_name: proposal.tool_name,
            params: proposal.params,
            rationale: proposal.rationale,
        }),
        local_state: next_local_state,
    };
    let artifact = Artifact::from_json(ArtifactKind::ExecThreadOutput, &output)?;
    let artifact_id = kernel.artifact_store.put(&artifact)?;
    let mut output = output;
    output.artifact_id = artifact_id;

    let _ = kernel.event_tx.send(exoskeleton_core::LiveEvent {
        event_type: EventType::ThreadRan,
        summary: format!("Exec thread {}: {}", spec.name, output.summary),
        ..exoskeleton_core::LiveEvent::new(Some(snapshot.tick_number + 1))
    });

    Ok(output)
}

fn build_coding_exec_prompt(
    spec: &ExecThreadSpec,
    kernel: &KernelContext,
    snapshot: &StateSnapshot,
    perception: &PerceptionResult,
    local_state: &ExecThreadLocalState,
) -> String {
    let recent_messages = perception
        .active_conversations
        .iter()
        .map(|c| c.message_refs.len())
        .sum::<usize>();
    let conversation_context = render_conversation_context(kernel, perception);
    let pending_results = render_pending_action_results(kernel, perception);
    format!(
        "Kind: {:?}\nName: {}\nCharter: {}\nMission: {}\nTick: {}\nWorkspace: {}\nLast action: {}\nCurrent focus: {}\nWork phase: {}\nScratchpad: {}\nEvidence complete: {}\nVerification pending: {}\nVerification attempted: {}\nRecent thread outputs: {}\nRecent exec outputs: {}\nRecent messages: {}\n\nConversation context:\n{}\n\nPending action results:\n{}\n\nTool parameter schemas:\n- code.read params: {{\"file_path\": \"ABSOLUTE_PATH\", \"offset\": optional_integer, \"limit\": optional_integer}}\n- code.grep params: {{\"pattern\": \"REGEX\", \"path\": \"ABSOLUTE_DIR\", \"output_mode\": optional_string}}\n- code.edit params: {{\"file_path\": \"ABSOLUTE_PATH\", \"old_string\": \"EXACT_TEXT_FROM_RECENT_READ\", \"new_string\": \"REPLACEMENT_TEXT\", \"replace_all\": optional_boolean}}\n- code.write params: {{\"file_path\": \"ABSOLUTE_PATH\", \"content\": \"FULL_FILE_CONTENT\"}}\n\nRules:\n- If there is no active coding work in the conversation or action feedback, return status=\"idle\" and proposed_action=null.\n- If the task is complete based on the conversation request and recent action results, set status=\"idle\", set completion_reason, and do not propose another action.\n- If a successful action already satisfied the task, prefer completion over redundant reads.\n- Use the workspace path exactly as given for any proposed code tool params.\n- Prefer code.edit for localized changes. Use code.write only for full-file replacement.\n- Never use `path` for code.read, code.edit, or code.write. Use `file_path`.\n- Never use `new_content` for code.edit. code.edit must use `old_string` and `new_string` copied exactly from a recent read result.\n- When you have enough evidence to perform the next edit safely, set evidence_complete=true.\n- Set proposal_confidence to low, medium, or high. Use high only when the next action is directly supported by recent reads/searches/action feedback.\n- After a successful mutating action, prefer either completion or one explicit verification action. Do not restart broad exploration.\n- If verification already succeeded, transition to idle rather than proposing more exploratory reads.\n- For typo or rename tasks, propose the smallest exact replacement needed rather than rewriting the file.\n\nRespond as JSON with fields: status, summary, current_focus, work_phase, scratchpad, evidence_complete, proposal_confidence, should_wake_master, completion_reason, proposed_action.\n`status` should usually be idle, active, or blocked.\n`proposed_action` must be null or an object with tool_name, params, rationale.",
        spec.kind,
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
        perception.thread_outputs.len(),
        perception.exec_thread_outputs.len(),
        recent_messages,
        conversation_context,
        pending_results,
    )
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

fn should_run_exec_thread(
    spec: &ExecThreadSpec,
    status: ExecThreadStatus,
    perception: &PerceptionResult,
    local_state: &ExecThreadLocalState,
) -> bool {
    match spec.kind {
        ExecThreadKind::Coding => match status {
            ExecThreadStatus::Failed => false,
            ExecThreadStatus::Active | ExecThreadStatus::Blocked | ExecThreadStatus::Completed => {
                true
            }
            ExecThreadStatus::Idle => {
                has_coding_work(perception, local_state) || local_state.should_wake_master
            }
        },
    }
}

fn has_coding_work(perception: &PerceptionResult, local_state: &ExecThreadLocalState) -> bool {
    !perception.new_messages.is_empty()
        || (local_state.awaiting_feedback && !perception.pending_action_results.is_empty())
}

fn default_coding_status(
    parsed: &CodingExecResponse,
    perception: &PerceptionResult,
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

fn normalize_coding_proposal(mut proposal: CodingExecProposal) -> Option<CodingExecProposal> {
    let params = proposal.params.as_object_mut()?;

    match proposal.tool_name.as_str() {
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
        "code.grep" => params.contains_key("pattern").then_some(proposal),
        _ => Some(proposal),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CodingFeedbackState {
    mutating_success: bool,
    mutating_failure: bool,
    verification_success: bool,
}

fn coding_feedback_from_latest_tick(
    kernel: &KernelContext,
    thread_id: exoskeleton_core::ThreadId,
) -> CodingFeedbackState {
    let Some(tick) = kernel.tick_store.latest().ok().flatten() else {
        return CodingFeedbackState::default();
    };

    let mut state = CodingFeedbackState::default();
    for action in tick.actions_taken {
        if action.origin_exec_thread_id != Some(thread_id) {
            continue;
        }
        match (action.action_type.as_str(), action.outcome) {
            (
                "code.edit" | "code.write" | "code.apply_patch" | "fs.write",
                exoskeleton_core::ActionOutcome::Success,
            ) => {
                state.mutating_success = true;
            }
            ("code.edit" | "code.write" | "code.apply_patch" | "fs.write", _) => {
                state.mutating_failure = true;
            }
            (
                "code.read" | "code.grep" | "shell.exec",
                exoskeleton_core::ActionOutcome::Success,
            ) => {
                state.verification_success = true;
            }
            _ => {}
        }
    }
    state
}

fn apply_coding_feedback_state(
    status: &mut ExecThreadStatus,
    local_state: &mut ExecThreadLocalState,
    feedback: &CodingFeedbackState,
    parsed: &mut CodingExecResponse,
) {
    if feedback.mutating_failure {
        local_state.verification_pending = false;
        local_state.verification_attempted = false;
        local_state.work_phase = Some("editing".into());
        if parsed.proposal_confidence.is_none() {
            parsed.proposal_confidence = Some(ExecThreadProposalConfidence::Medium);
        }
        *status = ExecThreadStatus::Active;
        return;
    }

    if feedback.mutating_success {
        local_state.verification_pending = true;
        local_state.verification_attempted = false;
        local_state.work_phase = Some("verifying".into());
    }

    if local_state.verification_pending && is_verification_action(parsed.proposed_action.as_ref()) {
        local_state.verification_attempted = true;
    }

    if local_state.verification_pending && feedback.verification_success {
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

    if local_state.verification_pending
        && local_state.verification_attempted
        && parsed
            .proposed_action
            .as_ref()
            .is_some_and(|proposal| is_exploratory_tool(&proposal.tool_name))
    {
        parsed.proposed_action = None;
        parsed.should_wake_master = false;
        *status = ExecThreadStatus::Idle;
    }
}

fn is_verification_action(proposal: Option<&CodingExecProposal>) -> bool {
    proposal
        .map(|proposal| {
            matches!(
                proposal.tool_name.as_str(),
                "code.read" | "code.grep" | "shell.exec"
            )
        })
        .unwrap_or(false)
}

fn is_exploratory_tool(tool_name: &str) -> bool {
    matches!(tool_name, "code.read" | "code.grep" | "code.ls")
}

fn render_conversation_context(kernel: &KernelContext, perception: &PerceptionResult) -> String {
    let mut out = String::new();
    for conversation in &perception.active_conversations {
        for msg in conversation.message_refs.iter().rev().take(3).rev() {
            let text = kernel
                .artifact_store
                .get(&msg.payload_ref)
                .ok()
                .flatten()
                .and_then(|artifact| String::from_utf8(artifact.content).ok())
                .unwrap_or_else(|| "<unresolved message>".into());
            let _ = writeln!(out, "- [{}] {}", msg.source, text.trim());
        }
    }
    if out.trim().is_empty() {
        "none".into()
    } else {
        out
    }
}

fn render_pending_action_results(kernel: &KernelContext, perception: &PerceptionResult) -> String {
    let mut out = String::new();
    for event in &perception.pending_action_results {
        let _ = writeln!(out, "- {}", event.summary);
        if let Some(payload_ref) = &event.payload_ref {
            if let Some(artifact) = kernel.artifact_store.get(payload_ref).ok().flatten() {
                if let Ok(text) = String::from_utf8(artifact.content.clone()) {
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
    }
    if out.trim().is_empty() {
        "none".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{
        conversation::Conversation, ArtifactId, EnvelopeId, EnvelopeKind, EventEntry, EventType,
        LedgerEntryId, MessageEnvelope, PrincipalId, TickId,
    };

    use super::*;

    fn coding_spec() -> ExecThreadSpec {
        ExecThreadSpec {
            thread_id: exoskeleton_core::ThreadId::new(),
            kind: ExecThreadKind::Coding,
            name: "Coding".into(),
            charter: "charter".into(),
            token_budget: 1000,
            workspace_root: None,
        }
    }

    fn perception_with(
        new_messages: Vec<MessageEnvelope>,
        pending_action_results: Vec<EventEntry>,
    ) -> PerceptionResult {
        PerceptionResult {
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
    fn canonicalize_completed_status_to_idle() {
        assert_eq!(
            canonicalize_exec_thread_status(ExecThreadStatus::Completed),
            ExecThreadStatus::Idle
        );
    }

    #[test]
    fn idle_coding_thread_does_not_reactivate_for_unrelated_action_feedback() {
        let spec = coding_spec();
        let local_state = ExecThreadLocalState::default();
        let perception = perception_with(vec![], vec![action_result_event()]);

        assert!(!should_run_exec_thread(
            &spec,
            ExecThreadStatus::Idle,
            &perception,
            &local_state
        ));
    }

    #[test]
    fn idle_coding_thread_reactivates_when_awaiting_feedback() {
        let spec = coding_spec();
        let local_state = ExecThreadLocalState {
            awaiting_feedback: true,
            ..ExecThreadLocalState::default()
        };
        let perception = perception_with(vec![], vec![action_result_event()]);

        assert!(should_run_exec_thread(
            &spec,
            ExecThreadStatus::Idle,
            &perception,
            &local_state
        ));
    }

    #[test]
    fn idle_coding_thread_reactivates_for_new_messages() {
        let spec = coding_spec();
        let local_state = ExecThreadLocalState::default();
        let perception = perception_with(vec![test_message()], vec![]);

        assert!(should_run_exec_thread(
            &spec,
            ExecThreadStatus::Idle,
            &perception,
            &local_state
        ));
    }
}
