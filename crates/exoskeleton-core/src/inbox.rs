//! Inbox trait — input channel for the vessel's Perceive step.

use crate::envelope::MessageEnvelope;
use crate::id::EnvelopeId;
use crate::ExoError;

/// Input channel for the vessel's Perceive step.
///
/// External actors (humans, other agents, system events) deposit
/// `MessageEnvelope`s into the inbox. The Perceive step calls `receive()`
/// to gather pending messages and `acknowledge()` to mark them consumed.
///
/// Implementations must be thread-safe (`Send + Sync`). The master loop
/// calls `receive()` once per tick from the Cognitive AQ handler thread.
pub trait Inbox: Send + Sync {
    /// Retrieve all pending (unacknowledged) messages.
    ///
    /// Returns messages in arrival order (oldest first). Messages remain
    /// pending until explicitly acknowledged via `acknowledge()`.
    fn receive(&self) -> Result<Vec<MessageEnvelope>, ExoError>;

    /// Acknowledge messages by their envelope IDs.
    ///
    /// Acknowledged messages are removed from the pending set and will
    /// not be returned by future `receive()` calls. Acknowledging an
    /// already-acknowledged or unknown ID is a no-op (idempotent).
    fn acknowledge(&self, ids: &[EnvelopeId]) -> Result<(), ExoError>;

    /// Submit a new message envelope to the inbox.
    ///
    /// The message will be available on the next `receive()` call (i.e., the
    /// next Perceive step). This is the write path for daemon and CLI `send`
    /// commands.
    ///
    /// Default: returns `ExoError::Config("inbox does not support submit")`.
    fn submit(&self, envelope: &MessageEnvelope) -> Result<(), ExoError> {
        let _ = envelope;
        Err(ExoError::Config("inbox does not support submit".into()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn inbox_trait_is_object_safe() {
        // This test verifies that Inbox can be used as a trait object.
        // If it compiles, the trait is object-safe.
        fn _accept_inbox(_inbox: Arc<dyn Inbox>) {}
    }

    /// A minimal inbox implementation that doesn't override submit().
    struct NoSubmitInbox;
    impl Inbox for NoSubmitInbox {
        fn receive(&self) -> Result<Vec<crate::MessageEnvelope>, crate::ExoError> {
            Ok(vec![])
        }
        fn acknowledge(&self, _ids: &[EnvelopeId]) -> Result<(), crate::ExoError> {
            Ok(())
        }
    }

    #[test]
    fn inbox_default_submit_returns_error() {
        let inbox = NoSubmitInbox;
        let env = crate::MessageEnvelope {
            id: EnvelopeId::new(),
            source: crate::PrincipalId::new(),
            target: None,
            kind: crate::EnvelopeKind::HumanMessage,
            payload_ref: crate::ArtifactId::from_content(b"test"),
            timestamp: chrono::Utc::now(),
            in_reply_to: None,
        };
        let result = inbox.submit(&env);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("does not support submit"));
    }
}
