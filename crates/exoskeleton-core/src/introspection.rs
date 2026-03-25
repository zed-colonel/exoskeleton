//! Introspective query types for the Decide step.
//!
//! These are cognitive self-awareness — read-only access to the vessel's
//! internal state. They run on the Cognitive AQ (synchronous SQLite reads),
//! NOT on the Tool AQ. This is not external I/O.

use serde::{Deserialize, Serialize};

/// Typed queries that the Decide step can issue against the vessel's own stores.
///
/// Each variant maps to a specific store query. Results are returned as
/// `serde_json::Value` — formatted as human-readable JSON for LLM consumption.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum IntrospectionQuery {
    /// Recent tick records with action counts, success rates, and durations.
    TickHistory {
        #[serde(default = "default_limit")]
        limit: u32,
    },
    /// Full detail for a specific tick.
    TickDetail { tick_id: String },
    /// Events filtered by optional type.
    EventHistory {
        #[serde(default)]
        event_type: Option<String>,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    /// Current trust levels for all known principals.
    TrustScores,
    /// Trust change history for one principal.
    TrustHistory {
        principal_id: String,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    /// Current budget dimensions with consumed/remaining.
    BudgetStatus,
    /// All registered threads with statuses and recent output summaries.
    ThreadStatus,
    /// Search long-term memory by optional topic and tags.
    MemorySearch {
        #[serde(default)]
        topic: Option<String>,
        #[serde(default)]
        tags: Option<Vec<String>>,
    },
    /// Full descriptor (including schemas) for a named connector.
    ConnectorDetails { name: String },
    /// All active watches (empty until E5-S2 adds watches).
    WatchList,
}

fn default_limit() -> u32 {
    20
}

/// Maximum allowed limit for any query (prevents unbounded reads).
pub const MAX_INTROSPECTION_LIMIT: u32 = 100;

#[cfg(test)]
mod tests {
    use super::*;

    // E5S1-T1: All 10 variants serialize/deserialize with correct `query` tag
    #[test]
    fn introspection_query_serde_roundtrip() {
        let variants = vec![
            IntrospectionQuery::TickHistory { limit: 10 },
            IntrospectionQuery::TickDetail {
                tick_id: "abc".into(),
            },
            IntrospectionQuery::EventHistory {
                event_type: Some("tick_started".into()),
                limit: 5,
            },
            IntrospectionQuery::TrustScores,
            IntrospectionQuery::TrustHistory {
                principal_id: "pid".into(),
                limit: 20,
            },
            IntrospectionQuery::BudgetStatus,
            IntrospectionQuery::ThreadStatus,
            IntrospectionQuery::MemorySearch {
                topic: Some("deploy".into()),
                tags: None,
            },
            IntrospectionQuery::ConnectorDetails {
                name: "http.request".into(),
            },
            IntrospectionQuery::WatchList,
        ];
        for variant in &variants {
            let json = serde_json::to_string(variant).unwrap();
            let parsed: IntrospectionQuery = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, variant);
        }
    }

    // E5S1-T2: Missing `limit` field deserializes to default_limit() (20)
    #[test]
    fn introspection_query_default_limit() {
        let json = r#"{"query":"tick_history"}"#;
        let parsed: IntrospectionQuery = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, IntrospectionQuery::TickHistory { limit: 20 });

        let json2 = r#"{"query":"event_history"}"#;
        let parsed2: IntrospectionQuery = serde_json::from_str(json2).unwrap();
        assert_eq!(
            parsed2,
            IntrospectionQuery::EventHistory {
                event_type: None,
                limit: 20,
            }
        );

        let json3 = r#"{"query":"trust_history","principal_id":"abc"}"#;
        let parsed3: IntrospectionQuery = serde_json::from_str(json3).unwrap();
        assert_eq!(
            parsed3,
            IntrospectionQuery::TrustHistory {
                principal_id: "abc".into(),
                limit: 20,
            }
        );
    }
}
