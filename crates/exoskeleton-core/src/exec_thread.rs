//! Executable thread domain primitives.
//!
//! Executable threads are proposal-producing workers that maintain local
//! work state between ticks. They do not invoke external tools directly;
//! the master loop remains the sole authority that chooses external actions.

use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, ThreadId, TickId};

/// Distinguishes executable thread roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecThreadKind {
    /// Coding and workspace-change proposal thread.
    Coding,
}

/// Operational status of an executable thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecThreadStatus {
    /// Thread is registered and quiescent, waiting for a new work item or
    /// fresh feedback tied to its current work item.
    Idle,
    /// Thread is actively pursuing a work item and may propose follow-up work.
    Active,
    /// Thread cannot make forward progress without more input or capability.
    Blocked,
    /// Legacy compatibility state. New runtime code should normalize this to
    /// `Idle` after recording the completion reason in local state.
    Completed,
    /// Thread failed and requires explicit operator or system intervention.
    Failed,
}

impl ExecThreadStatus {
    /// Whether this status is terminal without an explicit reset.
    ///
    /// `Completed` remains terminal here only for compatibility with older
    /// persisted states. New runtime code should normalize it to `Idle` once
    /// the completion reason has been captured.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Declared executable thread configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecThreadSpec {
    pub thread_id: ThreadId,
    pub kind: ExecThreadKind,
    pub name: String,
    pub charter: String,
    pub token_budget: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// Thread-local durable scratch/work state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecThreadLocalState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratchpad: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completion_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_phase: Option<String>,
    #[serde(default)]
    pub evidence_complete: bool,
    #[serde(default)]
    pub verification_pending: bool,
    #[serde(default)]
    pub verification_attempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_confidence: Option<ExecThreadProposalConfidence>,
    #[serde(default)]
    pub awaiting_feedback: bool,
    #[serde(default)]
    pub should_wake_master: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecThreadProposalConfidence {
    Low,
    Medium,
    High,
}

/// Candidate action proposed by an executable thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecThreadProposal {
    pub proposal_id: String,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub rationale: String,
}

/// Output artifact produced by an executable thread during a tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecThreadOutput {
    pub thread_id: ThreadId,
    pub tick_id: TickId,
    pub artifact_id: ArtifactId,
    pub kind: ExecThreadKind,
    pub summary: String,
    pub status: ExecThreadStatus,
    #[serde(default)]
    pub evidence_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_confidence: Option<ExecThreadProposalConfidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_action: Option<ExecThreadProposal>,
    #[serde(default)]
    pub local_state: ExecThreadLocalState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_thread_status_terminal() {
        assert!(ExecThreadStatus::Completed.is_terminal());
        assert!(ExecThreadStatus::Failed.is_terminal());
        assert!(!ExecThreadStatus::Idle.is_terminal());
        assert!(!ExecThreadStatus::Active.is_terminal());
        assert!(!ExecThreadStatus::Blocked.is_terminal());
    }
}
