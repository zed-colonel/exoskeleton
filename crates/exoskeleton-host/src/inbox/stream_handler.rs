//! InboxStreamHandler — bridges WI streaming messages to the vessel's inbox.

use std::sync::Arc;

use exoskeleton_core::artifact::{Artifact, ArtifactKind};
use exoskeleton_core::envelope::{EnvelopeKind, MessageEnvelope};
use exoskeleton_core::id::{derive_external_principal_id, EnvelopeId};
use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::ArtifactStore;
use worldinterface_core::streaming::{StreamMessage, StreamMessageHandler};

/// Bridges streaming messages from WI to the vessel's inbox.
///
/// For each incoming message:
/// 1. Derives PrincipalId from external identity
/// 2. Stores content as artifact
/// 3. Creates MessageEnvelope
/// 4. Writes to inbox (picked up by Perceive on next tick)
pub struct InboxStreamHandler {
    /// The vessel's inbox (FileInbox in production).
    inbox: Arc<dyn Inbox>,
    /// Artifact store for content-addressed storage.
    artifact_store: Arc<dyn ArtifactStore>,
}

impl InboxStreamHandler {
    /// Create a new handler with the given inbox and artifact store.
    pub fn new(inbox: Arc<dyn Inbox>, artifact_store: Arc<dyn ArtifactStore>) -> Self {
        Self {
            inbox,
            artifact_store,
        }
    }
}

impl StreamMessageHandler for InboxStreamHandler {
    fn handle_messages(
        &self,
        connector_name: &str,
        messages: Vec<StreamMessage>,
    ) -> Result<(), String> {
        for msg in messages {
            // 1. Derive PrincipalId
            let principal_id = derive_external_principal_id(&msg.source_identity);

            // 2. Store content as artifact
            let artifact = Artifact::new(
                ArtifactKind::Envelope,
                msg.content.as_bytes().to_vec(),
                "text/plain".to_string(),
            );
            let artifact_id = self
                .artifact_store
                .put(&artifact)
                .map_err(|e| format!("artifact store: {e}"))?;

            // 3. Create MessageEnvelope
            let envelope = MessageEnvelope {
                id: EnvelopeId::new(),
                source: principal_id,
                target: None,
                kind: EnvelopeKind::HumanMessage,
                payload_ref: artifact_id,
                timestamp: chrono::Utc::now(),
                in_reply_to: None,
            };

            // 4. Write to inbox
            self.inbox
                .submit(&envelope)
                .map_err(|e| format!("inbox submit: {e}"))?;

            tracing::debug!(
                connector = connector_name,
                source = %msg.source_identity,
                principal = %principal_id,
                "streaming message delivered to inbox"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use exoskeleton_core::artifact::{Artifact, ArtifactRef};
    use exoskeleton_core::id::ArtifactId;
    use exoskeleton_core::ExoError;

    use super::*;

    /// Minimal in-memory artifact store for testing.
    struct TestArtifactStore {
        artifacts: Mutex<HashMap<ArtifactId, Artifact>>,
    }

    impl TestArtifactStore {
        fn new() -> Self {
            Self {
                artifacts: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ArtifactStore for TestArtifactStore {
        fn put(&self, artifact: &Artifact) -> Result<ArtifactId, ExoError> {
            let id = artifact.id.clone();
            self.artifacts
                .lock()
                .unwrap()
                .insert(id.clone(), artifact.clone());
            Ok(id)
        }

        fn get(&self, id: &ArtifactId) -> Result<Option<Artifact>, ExoError> {
            Ok(self.artifacts.lock().unwrap().get(id).cloned())
        }

        fn exists(&self, id: &ArtifactId) -> Result<bool, ExoError> {
            Ok(self.artifacts.lock().unwrap().contains_key(id))
        }

        fn list_by_kind(
            &self,
            _kind: ArtifactKind,
            _limit: usize,
        ) -> Result<Vec<ArtifactRef>, ExoError> {
            Ok(vec![])
        }
    }

    // ── E4S3-T18: inbox_stream_handler_writes_envelope ──

    #[test]
    fn inbox_stream_handler_writes_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox: Arc<dyn Inbox> =
            Arc::new(crate::inbox::FileInbox::new(tmp.path().join("inbox")).unwrap());
        let artifact_store: Arc<dyn ArtifactStore> = Arc::new(TestArtifactStore::new());
        let handler = InboxStreamHandler::new(inbox.clone(), artifact_store.clone());

        let messages = vec![StreamMessage {
            source_identity: "discord:user:12345".into(),
            content: "hello from discord".into(),
            metadata: HashMap::from([("channel_id".into(), "ch-100".into())]),
        }];

        handler.handle_messages("discord", messages).unwrap();

        let envelopes = inbox.receive().unwrap();
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].kind, EnvelopeKind::HumanMessage);
        assert!(artifact_store.exists(&envelopes[0].payload_ref).unwrap());
    }

    // ── E4S3-T19: inbox_stream_handler_derives_principal ──

    #[test]
    fn inbox_stream_handler_derives_principal() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox: Arc<dyn Inbox> =
            Arc::new(crate::inbox::FileInbox::new(tmp.path().join("inbox")).unwrap());
        let artifact_store: Arc<dyn ArtifactStore> = Arc::new(TestArtifactStore::new());
        let handler = InboxStreamHandler::new(inbox.clone(), artifact_store);

        let messages = vec![StreamMessage {
            source_identity: "discord:user:99999".into(),
            content: "test".into(),
            metadata: HashMap::new(),
        }];

        handler.handle_messages("discord", messages).unwrap();

        let envelopes = inbox.receive().unwrap();
        assert_eq!(envelopes.len(), 1);
        let expected_principal = derive_external_principal_id("discord:user:99999");
        assert_eq!(envelopes[0].source, expected_principal);
    }

    // ── E4S3-T20: inbox_stream_handler_stores_artifact ──

    #[test]
    fn inbox_stream_handler_stores_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox: Arc<dyn Inbox> =
            Arc::new(crate::inbox::FileInbox::new(tmp.path().join("inbox")).unwrap());
        let artifact_store: Arc<dyn ArtifactStore> = Arc::new(TestArtifactStore::new());
        let handler = InboxStreamHandler::new(inbox.clone(), artifact_store.clone());

        let content = "important message content";
        let messages = vec![StreamMessage {
            source_identity: "webhook:alerts".into(),
            content: content.into(),
            metadata: HashMap::new(),
        }];

        handler.handle_messages("webhook", messages).unwrap();

        let envelopes = inbox.receive().unwrap();
        assert_eq!(envelopes.len(), 1);

        // Verify artifact content matches
        let expected_artifact_id = ArtifactId::from_content(content.as_bytes());
        assert_eq!(envelopes[0].payload_ref, expected_artifact_id);

        let artifact = artifact_store
            .get(&envelopes[0].payload_ref)
            .unwrap()
            .unwrap();
        assert_eq!(artifact.content, content.as_bytes());
        assert_eq!(artifact.content_type, "text/plain");
    }
}
