//! Message envelope types for inter-agent and operator communications.
//!
//! All inbound/outbound communications are envelopes referencing artifacts.
//! Envelopes never contain large inline payloads — they reference artifacts
//! by [`ArtifactId`].

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, EnvelopeId, PrincipalId};
use crate::relationship::RelationalSignalType;

/// Typed inbound/outbound message.
///
/// All inter-agent and operator communications are envelopes referencing
/// artifacts. Envelopes never contain large inline payloads — they reference
/// artifacts by `ArtifactId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEnvelope {
    /// Unique identity of this envelope.
    pub id: EnvelopeId,
    /// Who sent this message.
    pub source: PrincipalId,
    /// Intended recipient. `None` = broadcast or self-directed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PrincipalId>,
    /// What kind of message this is.
    pub kind: EnvelopeKind,
    /// Reference to the artifact containing the full payload.
    pub payload_ref: ArtifactId,
    /// When this envelope was created.
    pub timestamp: DateTime<Utc>,
    /// Reference to the envelope this is a reply to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<EnvelopeId>,
}

/// Classification of message envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    /// Message from a human operator.
    HumanMessage,
    /// Message from another agent.
    AgentMessage,
    /// A system-generated event (timer, threshold, error notification).
    SystemEvent,
    /// A relational signal (trust update, feedback, alignment check).
    RelationalSignal,
}

/// A relational signal carried in an envelope.
///
/// Every relational signal arrives as typed envelope -> artifact -> ledger append
/// (IBP §4.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationalSignal {
    /// What type of relational signal this is.
    pub signal_type: RelationalSignalType,
    /// Which principal this signal concerns.
    pub principal_id: PrincipalId,
    /// Human-readable description of the signal.
    pub content: String,
    /// Extensible metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-6: Message Envelope ──

    #[test]
    fn envelope_json_roundtrip_full() {
        let env = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: Some(PrincipalId::new()),
            kind: EnvelopeKind::HumanMessage,
            payload_ref: ArtifactId::from_content(b"payload"),
            timestamp: Utc::now(),
            in_reply_to: Some(EnvelopeId::new()),
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed: MessageEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, parsed);
    }

    #[test]
    fn envelope_json_roundtrip_minimal() {
        let env = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::SystemEvent,
            payload_ref: ArtifactId::from_content(b"event"),
            timestamp: Utc::now(),
            in_reply_to: None,
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed: MessageEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, parsed);
    }

    #[test]
    fn envelope_optional_fields_omitted() {
        let env = MessageEnvelope {
            id: EnvelopeId::new(),
            source: PrincipalId::new(),
            target: None,
            kind: EnvelopeKind::AgentMessage,
            payload_ref: ArtifactId::from_content(b"msg"),
            timestamp: Utc::now(),
            in_reply_to: None,
        };
        let value: serde_json::Value = serde_json::to_value(&env).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("target"));
        assert!(!obj.contains_key("in_reply_to"));
    }

    #[test]
    fn envelope_kind_all_variants_roundtrip() {
        let variants = [
            EnvelopeKind::HumanMessage,
            EnvelopeKind::AgentMessage,
            EnvelopeKind::SystemEvent,
            EnvelopeKind::RelationalSignal,
        ];
        for kind in &variants {
            let json = serde_json::to_string(kind).unwrap();
            let parsed: EnvelopeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, parsed);
        }
    }

    #[test]
    fn relational_signal_roundtrip() {
        let sig = RelationalSignal {
            signal_type: RelationalSignalType::TrustUpdate,
            principal_id: PrincipalId::new(),
            content: "Trust increased after successful delivery".into(),
            metadata: {
                let mut m = HashMap::new();
                m.insert("delta".into(), "+0.1".into());
                m
            },
        };
        let json = serde_json::to_string(&sig).unwrap();
        let parsed: RelationalSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(sig, parsed);
    }
}
