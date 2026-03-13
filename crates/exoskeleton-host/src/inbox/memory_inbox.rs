//! In-memory inbox for testing.

use std::sync::Mutex;

use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{EnvelopeId, ExoError, MessageEnvelope};

/// In-memory inbox for testing.
///
/// Messages are pushed via `push()` and consumed via the `Inbox` trait.
pub struct InMemoryInbox {
    pending: Mutex<Vec<MessageEnvelope>>,
    acknowledged: Mutex<Vec<EnvelopeId>>,
}

impl InMemoryInbox {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            acknowledged: Mutex::new(Vec::new()),
        }
    }

    /// Push a message into the inbox (test helper).
    pub fn push(&self, envelope: MessageEnvelope) {
        self.pending.lock().unwrap().push(envelope);
    }

    /// Get the list of acknowledged IDs (test helper).
    pub fn acknowledged_ids(&self) -> Vec<EnvelopeId> {
        self.acknowledged.lock().unwrap().clone()
    }
}

impl Default for InMemoryInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Inbox for InMemoryInbox {
    fn receive(&self) -> Result<Vec<MessageEnvelope>, ExoError> {
        let pending = self.pending.lock().unwrap();
        let acked = self.acknowledged.lock().unwrap();
        Ok(pending
            .iter()
            .filter(|env| !acked.contains(&env.id))
            .cloned()
            .collect())
    }

    fn acknowledge(&self, ids: &[EnvelopeId]) -> Result<(), ExoError> {
        let mut acked = self.acknowledged.lock().unwrap();
        for id in ids {
            if !acked.contains(id) {
                acked.push(*id);
            }
        }
        Ok(())
    }

    fn submit(&self, envelope: &MessageEnvelope) -> Result<(), ExoError> {
        self.pending.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{ArtifactId, EnvelopeId, EnvelopeKind, MessageEnvelope, PrincipalId};

    use super::*;

    fn test_envelope() -> MessageEnvelope {
        MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"test"),
            timestamp: Utc::now(),
            in_reply_to: None,
        }
    }

    #[test]
    fn in_memory_inbox_empty() {
        let inbox = InMemoryInbox::new();
        let messages = inbox.receive().unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn in_memory_inbox_push_receive() {
        let inbox = InMemoryInbox::new();
        let env = test_envelope();
        let id = env.id;
        inbox.push(env);

        let messages = inbox.receive().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, id);
    }

    #[test]
    fn in_memory_inbox_acknowledge() {
        let inbox = InMemoryInbox::new();
        let env = test_envelope();
        let id = env.id;
        inbox.push(env);

        // Before acknowledge, message is returned
        assert_eq!(inbox.receive().unwrap().len(), 1);

        // Acknowledge it
        inbox.acknowledge(&[id]).unwrap();

        // After acknowledge, message is no longer returned
        assert!(inbox.receive().unwrap().is_empty());
        assert_eq!(inbox.acknowledged_ids(), vec![id]);
    }

    #[test]
    fn in_memory_inbox_submit() {
        let inbox = InMemoryInbox::new();
        let env = test_envelope();
        let id = env.id;

        inbox.submit(&env).unwrap();

        let messages = inbox.receive().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, id);
    }

    #[test]
    fn in_memory_inbox_acknowledge_idempotent() {
        let inbox = InMemoryInbox::new();

        // Acknowledging an unknown ID is a no-op
        let unknown_id = EnvelopeId::new();
        inbox.acknowledge(&[unknown_id]).unwrap();
        assert_eq!(inbox.acknowledged_ids(), vec![unknown_id]);

        // Acknowledging the same ID again doesn't duplicate it
        inbox.acknowledge(&[unknown_id]).unwrap();
        assert_eq!(inbox.acknowledged_ids(), vec![unknown_id]);
    }
}
