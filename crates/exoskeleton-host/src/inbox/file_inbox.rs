//! File-based inbox that watches a directory for JSON envelope files.

use std::path::PathBuf;

use exoskeleton_core::inbox::Inbox;
use exoskeleton_core::{EnvelopeId, ExoError, MessageEnvelope};

/// File-based inbox that watches a directory for JSON envelope files.
///
/// Convention: files are named `{envelope_id}.json` where `envelope_id`
/// is the UUID from the MessageEnvelope. The Perceive step reads all
/// `.json` files in the inbox directory. Acknowledged messages are moved
/// to a `processed/` subdirectory.
pub struct FileInbox {
    inbox_dir: PathBuf,
    processed_dir: PathBuf,
}

impl FileInbox {
    /// Create a new FileInbox watching the given directory.
    ///
    /// Creates the directory and `processed/` subdirectory if they don't exist.
    pub fn new(inbox_dir: PathBuf) -> Result<Self, ExoError> {
        let processed_dir = inbox_dir.join("processed");
        std::fs::create_dir_all(&inbox_dir)?;
        std::fs::create_dir_all(&processed_dir)?;
        Ok(Self {
            inbox_dir,
            processed_dir,
        })
    }
}

impl Inbox for FileInbox {
    fn receive(&self) -> Result<Vec<MessageEnvelope>, ExoError> {
        let mut envelopes = Vec::new();
        let entries = std::fs::read_dir(&self.inbox_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") && path.is_file() {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str::<MessageEnvelope>(&contents) {
                        Ok(env) => envelopes.push(env),
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "skipping unparseable inbox file"
                            );
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "skipping unreadable inbox file"
                        );
                    }
                }
            }
        }
        // Sort by timestamp (oldest first)
        envelopes.sort_by_key(|e| e.timestamp);
        Ok(envelopes)
    }

    fn acknowledge(&self, ids: &[EnvelopeId]) -> Result<(), ExoError> {
        for id in ids {
            let src = self.inbox_dir.join(format!("{id}.json"));
            let dst = self.processed_dir.join(format!("{id}.json"));
            if src.exists() {
                std::fs::rename(&src, &dst)?;
            }
            // Missing file is silently ignored (idempotent)
        }
        Ok(())
    }

    fn submit(&self, envelope: &MessageEnvelope) -> Result<(), ExoError> {
        let json = serde_json::to_string_pretty(envelope)
            .map_err(|e| ExoError::Storage(format!("envelope serialize: {e}")))?;
        let path = self.inbox_dir.join(format!("{}.json", envelope.id));
        std::fs::write(&path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::inbox::Inbox;
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
    fn file_inbox_creates_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let inbox = FileInbox::new(inbox_dir.clone()).unwrap();

        assert!(inbox_dir.exists());
        assert!(inbox.processed_dir.exists());
    }

    #[test]
    fn file_inbox_reads_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let inbox = FileInbox::new(inbox_dir.clone()).unwrap();

        let env = test_envelope();
        let json = serde_json::to_string_pretty(&env).unwrap();
        let file_path = inbox_dir.join(format!("{}.json", env.id));
        std::fs::write(&file_path, &json).unwrap();

        let messages = inbox.receive().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, env.id);
    }

    #[test]
    fn file_inbox_submit_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let inbox = FileInbox::new(inbox_dir.clone()).unwrap();

        let env = test_envelope();
        let env_id = env.id;
        inbox.submit(&env).unwrap();

        let file_path = inbox_dir.join(format!("{env_id}.json"));
        assert!(file_path.exists(), "submit should create file in inbox dir");
    }

    #[test]
    fn file_inbox_submit_then_receive() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let inbox = FileInbox::new(inbox_dir.clone()).unwrap();

        let env = test_envelope();
        let env_id = env.id;
        inbox.submit(&env).unwrap();

        let messages = inbox.receive().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, env_id);
    }

    #[test]
    fn file_inbox_acknowledge_moves_to_processed() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let inbox = FileInbox::new(inbox_dir.clone()).unwrap();

        let env = test_envelope();
        let env_id = env.id;
        let json = serde_json::to_string_pretty(&env).unwrap();
        let file_path = inbox_dir.join(format!("{env_id}.json"));
        std::fs::write(&file_path, &json).unwrap();

        // File exists in inbox
        assert!(file_path.exists());

        // Acknowledge it
        inbox.acknowledge(&[env_id]).unwrap();

        // File moved to processed
        assert!(!file_path.exists());
        let processed_path = inbox.processed_dir.join(format!("{env_id}.json"));
        assert!(processed_path.exists());

        // Subsequent receive returns empty
        let messages = inbox.receive().unwrap();
        assert!(messages.is_empty());
    }
}
