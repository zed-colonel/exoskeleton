//! Charter governance types — proposals for thread charter modifications.
//!
//! Created by the Meta-Cognition thread when it detects cognitive patterns
//! that suggest charter improvements. Stored as content-addressed artifacts
//! and tracked via EventType::CharterProposal events.

use serde::{Deserialize, Serialize};

use crate::id::ThreadId;

/// A proposed modification to a thread's charter text.
///
/// Created by the Meta-Cognition thread when it detects cognitive patterns
/// that suggest charter improvements. Stored as a content-addressed artifact
/// and tracked via an EventType::CharterProposal event.
///
/// The lifecycle: Pending → Approved/Denied → Applied (if approved).
/// Only the operator can transition from Pending to Approved/Denied.
/// The apply step is a separate daemon action that updates the ThreadRegistry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CharterProposal {
    /// Which thread this proposal targets.
    pub thread_id: ThreadId,
    /// Human-readable thread name (for display).
    pub thread_name: String,
    /// The current charter text at time of proposal.
    pub current_charter: String,
    /// The proposed replacement charter text.
    pub proposed_charter: String,
    /// Why the Meta-Cognition thread thinks this change is warranted.
    pub rationale: String,
    /// Cognitive patterns that led to this proposal (evidence).
    pub detected_patterns: Vec<String>,
    /// Tick number when the proposal was generated.
    pub proposed_at_tick: u64,
    /// Current status of the proposal.
    pub status: ProposalStatus,
}

/// Lifecycle state of a charter proposal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Awaiting operator review.
    Pending,
    /// Operator approved the proposal.
    Approved,
    /// Operator denied the proposal.
    Denied,
    /// Approved proposal has been applied to the ThreadRegistry.
    Applied,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E5S2-T16: charter_proposal_serde_roundtrip ──

    #[test]
    fn charter_proposal_serde_roundtrip() {
        let proposal = CharterProposal {
            thread_id: ThreadId::new(),
            thread_name: "Self-Critique".into(),
            current_charter: "You are the Self-Critique thread...".into(),
            proposed_charter: "You are the Self-Critique thread with enhanced focus...".into(),
            rationale: "Decision quality has been declining".into(),
            detected_patterns: vec!["Repeated action failures".into(), "Low success rate".into()],
            proposed_at_tick: 42,
            status: ProposalStatus::Pending,
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let parsed: CharterProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(proposal, parsed);

        // All ProposalStatus variants
        for status in [
            ProposalStatus::Pending,
            ProposalStatus::Approved,
            ProposalStatus::Denied,
            ProposalStatus::Applied,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: ProposalStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }
    }
}
