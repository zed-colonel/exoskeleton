//! Relational signal processing — envelope to ledger pipeline.
//!
//! Processes incoming `RelationalSignal` envelopes from the Perceive step into
//! `RelationshipRecord` entries in the ledger. Follows IBP §4.4: every
//! relational signal arrives as typed envelope → artifact → ledger append.

use exoskeleton_core::{
    ArtifactStore, EnvelopeKind, LedgerEntryId, MessageEnvelope, RelationalSignal,
    RelationshipRecord, TickId,
};

use crate::ledger::RelationshipLedger;

/// Process relational signals from inbox messages into ledger entries.
///
/// For each `MessageEnvelope` with `kind == RelationalSignal`:
/// 1. Retrieve the signal artifact by `payload_ref`
/// 2. Parse as `RelationalSignal`
/// 3. Create a `RelationshipRecord`
/// 4. Append to the ledger atomically
///
/// Returns the list of newly created `LedgerEntryId`s.
///
/// Signals that fail to parse are logged and skipped (graceful degradation).
pub fn process_relational_signals(
    messages: &[MessageEnvelope],
    ledger: &dyn RelationshipLedger,
    artifact_store: &dyn ArtifactStore,
    tick_id: TickId,
) -> Vec<LedgerEntryId> {
    let mut entry_ids = Vec::new();

    for envelope in messages {
        if envelope.kind != EnvelopeKind::RelationalSignal {
            continue;
        }

        // 1. Retrieve the signal artifact
        let artifact = match artifact_store.get(&envelope.payload_ref) {
            Ok(Some(a)) => a,
            Ok(None) => {
                tracing::warn!(
                    envelope_id = %envelope.id,
                    artifact_id = %envelope.payload_ref,
                    "relational signal artifact not found — skipping"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    envelope_id = %envelope.id,
                    error = %e,
                    "failed to retrieve relational signal artifact — skipping"
                );
                continue;
            }
        };

        // 2. Parse as RelationalSignal
        let signal: RelationalSignal = match serde_json::from_slice(&artifact.content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    envelope_id = %envelope.id,
                    error = %e,
                    "failed to parse relational signal — skipping"
                );
                continue;
            }
        };

        // 3. Create a RelationshipRecord
        let record = RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id: signal.principal_id,
            signal_type: signal.signal_type,
            content_ref: envelope.payload_ref.clone(),
            tick_id,
            timestamp: envelope.timestamp,
            metadata: signal.metadata.clone(),
        };

        // 4. Append to the ledger
        match ledger.append(&record) {
            Ok(id) => entry_ids.push(id),
            Err(e) => {
                tracing::warn!(
                    envelope_id = %envelope.id,
                    error = %e,
                    "failed to append relational signal to ledger — skipping"
                );
            }
        }
    }

    entry_ids
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use chrono::Utc;
    use exoskeleton_core::{
        Artifact, ArtifactId, ArtifactKind, EnvelopeId, ExoError, PrincipalId, RelationalSignalType,
    };

    use super::*;
    use crate::ledger::InMemoryRelationshipLedger;

    /// Minimal in-memory artifact store for testing signal processing.
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
            let mut store = self.artifacts.lock().unwrap();
            store.insert(artifact.id.clone(), artifact.clone());
            Ok(artifact.id.clone())
        }

        fn get(&self, id: &ArtifactId) -> Result<Option<Artifact>, ExoError> {
            let store = self.artifacts.lock().unwrap();
            Ok(store.get(id).cloned())
        }

        fn exists(&self, id: &ArtifactId) -> Result<bool, ExoError> {
            let store = self.artifacts.lock().unwrap();
            Ok(store.contains_key(id))
        }

        fn list_by_kind(
            &self,
            kind: ArtifactKind,
            limit: usize,
        ) -> Result<Vec<exoskeleton_core::ArtifactRef>, ExoError> {
            let store = self.artifacts.lock().unwrap();
            let refs: Vec<_> = store
                .values()
                .filter(|a| a.kind == kind)
                .take(limit)
                .map(|a| exoskeleton_core::ArtifactRef {
                    id: a.id.clone(),
                    kind: a.kind,
                })
                .collect();
            Ok(refs)
        }
    }

    fn make_signal_envelope(
        signal: &RelationalSignal,
        artifact_store: &TestArtifactStore,
    ) -> MessageEnvelope {
        let artifact = Artifact::from_json(ArtifactKind::RelationshipEntry, signal).unwrap();
        let artifact_id = artifact_store.put(&artifact).unwrap();

        MessageEnvelope {
            id: EnvelopeId::new(),
            source: signal.principal_id,
            target: None,
            kind: EnvelopeKind::RelationalSignal,
            payload_ref: artifact_id,
            timestamp: Utc::now(),
            in_reply_to: None,
        }
    }

    // ── T-3: Signal Processing ──

    #[test]
    fn process_relational_signal_envelope() {
        let ledger = InMemoryRelationshipLedger::new();
        let artifact_store = TestArtifactStore::new();
        let tick_id = TickId::new();
        let principal = PrincipalId::new();

        let signal = RelationalSignal {
            signal_type: RelationalSignalType::TrustUpdate,
            principal_id: principal,
            content: "Trust increased".into(),
            metadata: HashMap::new(),
        };

        let envelope = make_signal_envelope(&signal, &artifact_store);
        let ids = process_relational_signals(&[envelope], &ledger, &artifact_store, tick_id);

        assert_eq!(ids.len(), 1);
        assert_eq!(ledger.count().unwrap(), 1);

        let records = ledger.for_principal(principal, 10).unwrap();
        assert_eq!(records[0].signal_type, RelationalSignalType::TrustUpdate);
        assert_eq!(records[0].tick_id, tick_id);
    }

    #[test]
    fn process_non_relational_envelope_ignored() {
        let ledger = InMemoryRelationshipLedger::new();
        let artifact_store = TestArtifactStore::new();
        let tick_id = TickId::new();

        let envelope = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"human-msg"),
            timestamp: Utc::now(),
            in_reply_to: None,
        };

        let ids = process_relational_signals(&[envelope], &ledger, &artifact_store, tick_id);
        assert!(ids.is_empty());
        assert_eq!(ledger.count().unwrap(), 0);
    }

    #[test]
    fn process_multiple_signals() {
        let ledger = InMemoryRelationshipLedger::new();
        let artifact_store = TestArtifactStore::new();
        let tick_id = TickId::new();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        let s1 = RelationalSignal {
            signal_type: RelationalSignalType::TrustUpdate,
            principal_id: p1,
            content: "Trust for p1".into(),
            metadata: HashMap::new(),
        };
        let s2 = RelationalSignal {
            signal_type: RelationalSignalType::FeedbackReceived,
            principal_id: p2,
            content: "Feedback from p2".into(),
            metadata: HashMap::new(),
        };

        let e1 = make_signal_envelope(&s1, &artifact_store);
        let e2 = make_signal_envelope(&s2, &artifact_store);

        let ids = process_relational_signals(&[e1, e2], &ledger, &artifact_store, tick_id);
        assert_eq!(ids.len(), 2);
        assert_eq!(ledger.count().unwrap(), 2);
    }

    #[test]
    fn invalid_artifact_content_graceful_skip() {
        let ledger = InMemoryRelationshipLedger::new();
        let artifact_store = TestArtifactStore::new();
        let tick_id = TickId::new();

        // Store an artifact with invalid JSON content
        let bad_artifact = Artifact::new(
            ArtifactKind::RelationshipEntry,
            b"not valid json".to_vec(),
            "application/json".into(),
        );
        let artifact_id = artifact_store.put(&bad_artifact).unwrap();

        let envelope = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::RelationalSignal,
            payload_ref: artifact_id,
            timestamp: Utc::now(),
            in_reply_to: None,
        };

        // Should not panic — graceful skip
        let ids = process_relational_signals(&[envelope], &ledger, &artifact_store, tick_id);
        assert!(ids.is_empty());
        assert_eq!(ledger.count().unwrap(), 0);
    }

    #[test]
    fn missing_artifact_graceful_skip() {
        let ledger = InMemoryRelationshipLedger::new();
        let artifact_store = TestArtifactStore::new();
        let tick_id = TickId::new();

        let envelope = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::RelationalSignal,
            payload_ref: ArtifactId::from_content(b"nonexistent"),
            timestamp: Utc::now(),
            in_reply_to: None,
        };

        // Should not panic — graceful skip
        let ids = process_relational_signals(&[envelope], &ledger, &artifact_store, tick_id);
        assert!(ids.is_empty());
        assert_eq!(ledger.count().unwrap(), 0);
    }
}
