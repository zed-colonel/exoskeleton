//! Working memory scratchpad model for Exoskeleton (W-12).
//!
//! A keyed, TTL-aware scratchpad that replaces the freeform String
//! working_context. Entries have keys for targeted read/write, TTL
//! for automatic expiry, and relevance scores for priority-based eviction.

use serde::{Deserialize, Serialize};

/// The working memory scratchpad — a desk, not a filing cabinet.
///
/// Entries persist across ticks (unlike context which is compiled
/// fresh per I5) but are not permanent (unlike long-term notes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct WorkingMemory {
    pub entries: Vec<WorkingMemoryEntry>,
}

impl Default for WorkingMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkingMemory {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Create from a legacy working context string (migration helper).
    pub fn from_legacy_string(text: String) -> Self {
        if text.is_empty() {
            return Self::new();
        }
        Self {
            entries: vec![WorkingMemoryEntry {
                key: "legacy_context".into(),
                value: text,
                written_at_tick: 0,
                ttl_ticks: None,
                relevance: 1.0,
            }],
        }
    }

    /// Apply a list of operations.
    pub fn apply_ops(&mut self, ops: &[WorkingMemoryOp], current_tick: u64) {
        for op in ops {
            self.apply_op(op, current_tick);
        }
    }

    /// Apply a single operation.
    pub fn apply_op(&mut self, op: &WorkingMemoryOp, current_tick: u64) {
        match op {
            WorkingMemoryOp::Set {
                key,
                value,
                ttl_ticks,
            } => {
                // Upsert: replace existing entry with same key, or add new
                if let Some(entry) = self.entries.iter_mut().find(|e| e.key == *key) {
                    entry.value = value.clone();
                    entry.written_at_tick = current_tick;
                    entry.ttl_ticks = *ttl_ticks;
                    entry.relevance = 1.0; // reset relevance on write
                } else {
                    self.entries.push(WorkingMemoryEntry {
                        key: key.clone(),
                        value: value.clone(),
                        written_at_tick: current_tick,
                        ttl_ticks: *ttl_ticks,
                        relevance: 1.0,
                    });
                }
            }
            WorkingMemoryOp::Remove { key } => {
                self.entries.retain(|e| e.key != *key);
            }
            WorkingMemoryOp::Clear => {
                self.entries.clear();
            }
        }
    }

    /// Evict entries whose TTL has expired.
    pub fn evict_expired(&mut self, current_tick: u64) {
        self.entries.retain(|entry| match entry.ttl_ticks {
            Some(ttl) => current_tick.saturating_sub(entry.written_at_tick) < ttl,
            None => true,
        });
    }

    /// Enforce entry cap by dropping lowest-relevance entries.
    pub fn enforce_cap(&mut self, max_entries: usize) {
        if self.entries.len() <= max_entries {
            return;
        }
        self.entries.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.entries.truncate(max_entries);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// A single working memory entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct WorkingMemoryEntry {
    pub key: String,
    pub value: String,
    pub written_at_tick: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ticks: Option<u64>,
    /// Relevance score [0.0, 1.0] for Context Compiler priority and eviction.
    pub relevance: f64,
}

/// Operation on working memory (emitted by Decide and Reflect steps).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WorkingMemoryOp {
    Set {
        key: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_ticks: Option<u64>,
    },
    Remove {
        key: String,
    },
    Clear,
}

/// Custom deserializer: handles legacy string and new WorkingMemory struct.
///
/// - `"working_context": "text"` -> `WorkingMemory::from_legacy_string("text")`
/// - `"working_memory": {"entries": [...]}` -> `WorkingMemory { entries: [...] }`
/// - `""` or absent -> `WorkingMemory::new()`
pub fn deserialize_working_memory_compat<'de, D>(deserializer: D) -> Result<WorkingMemory, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let value: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(WorkingMemory::new()),
        serde_json::Value::String(s) => Ok(WorkingMemory::from_legacy_string(s)),
        v @ serde_json::Value::Object(_) => {
            serde_json::from_value(v).map_err(serde::de::Error::custom)
        }
        other => Err(serde::de::Error::custom(format!(
            "expected string or object for working_memory, got: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E1-T21: WorkingMemory JSON roundtrip ──
    #[test]
    fn working_memory_json_roundtrip() {
        let wm = WorkingMemory {
            entries: vec![
                WorkingMemoryEntry {
                    key: "goal".into(),
                    value: "Find the answer".into(),
                    written_at_tick: 5,
                    ttl_ticks: Some(10),
                    relevance: 0.9,
                },
                WorkingMemoryEntry {
                    key: "context".into(),
                    value: "User asked about X".into(),
                    written_at_tick: 3,
                    ttl_ticks: None,
                    relevance: 0.5,
                },
            ],
        };
        let json = serde_json::to_string(&wm).unwrap();
        let parsed: WorkingMemory = serde_json::from_str(&json).unwrap();
        assert_eq!(wm, parsed);
    }

    // ── E1-T22: WorkingMemoryOp all 3 variants roundtrip ──
    #[test]
    fn working_memory_op_all_variants_roundtrip() {
        let ops = vec![
            WorkingMemoryOp::Set {
                key: "k".into(),
                value: "v".into(),
                ttl_ticks: Some(5),
            },
            WorkingMemoryOp::Remove { key: "k".into() },
            WorkingMemoryOp::Clear,
        ];
        for op in &ops {
            let json = serde_json::to_string(op).unwrap();
            let parsed: WorkingMemoryOp = serde_json::from_str(&json).unwrap();
            assert_eq!(*op, parsed);
        }
        // Verify tagged enum format
        let set_json = serde_json::to_string(&ops[0]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&set_json).unwrap();
        assert_eq!(value["op"], "set");
    }

    // ── E1-T23: apply_op(Set) creates new entry ──
    #[test]
    fn apply_op_set_creates_new() {
        let mut wm = WorkingMemory::new();
        wm.apply_op(
            &WorkingMemoryOp::Set {
                key: "goal".into(),
                value: "Find answer".into(),
                ttl_ticks: Some(10),
            },
            5,
        );
        assert_eq!(wm.entries.len(), 1);
        assert_eq!(wm.entries[0].key, "goal");
        assert_eq!(wm.entries[0].value, "Find answer");
        assert_eq!(wm.entries[0].written_at_tick, 5);
        assert_eq!(wm.entries[0].ttl_ticks, Some(10));
        assert_eq!(wm.entries[0].relevance, 1.0);
    }

    // ── E1-T24: apply_op(Set) upserts existing entry ──
    #[test]
    fn apply_op_set_upserts_existing() {
        let mut wm = WorkingMemory {
            entries: vec![WorkingMemoryEntry {
                key: "goal".into(),
                value: "Old value".into(),
                written_at_tick: 1,
                ttl_ticks: Some(5),
                relevance: 0.3,
            }],
        };
        wm.apply_op(
            &WorkingMemoryOp::Set {
                key: "goal".into(),
                value: "New value".into(),
                ttl_ticks: Some(20),
            },
            10,
        );
        assert_eq!(wm.entries.len(), 1);
        assert_eq!(wm.entries[0].value, "New value");
        assert_eq!(wm.entries[0].written_at_tick, 10);
        assert_eq!(wm.entries[0].ttl_ticks, Some(20));
        assert_eq!(wm.entries[0].relevance, 1.0); // reset on write
    }

    // ── E1-T25: apply_op(Remove) deletes entry by key ──
    #[test]
    fn apply_op_remove() {
        let mut wm = WorkingMemory {
            entries: vec![
                WorkingMemoryEntry {
                    key: "a".into(),
                    value: "1".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 1.0,
                },
                WorkingMemoryEntry {
                    key: "b".into(),
                    value: "2".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 1.0,
                },
            ],
        };
        wm.apply_op(&WorkingMemoryOp::Remove { key: "a".into() }, 5);
        assert_eq!(wm.entries.len(), 1);
        assert_eq!(wm.entries[0].key, "b");
    }

    // ── E1-T26: apply_op(Clear) removes all entries ──
    #[test]
    fn apply_op_clear() {
        let mut wm = WorkingMemory {
            entries: vec![
                WorkingMemoryEntry {
                    key: "a".into(),
                    value: "1".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 1.0,
                },
                WorkingMemoryEntry {
                    key: "b".into(),
                    value: "2".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 1.0,
                },
            ],
        };
        wm.apply_op(&WorkingMemoryOp::Clear, 5);
        assert!(wm.is_empty());
    }

    // ── E1-T27: evict_expired removes expired, keeps non-expired ──
    #[test]
    fn evict_expired() {
        let mut wm = WorkingMemory {
            entries: vec![
                WorkingMemoryEntry {
                    key: "old".into(),
                    value: "expired".into(),
                    written_at_tick: 1,
                    ttl_ticks: Some(3), // expires at tick 4
                    relevance: 1.0,
                },
                WorkingMemoryEntry {
                    key: "new".into(),
                    value: "fresh".into(),
                    written_at_tick: 8,
                    ttl_ticks: Some(5), // expires at tick 13
                    relevance: 1.0,
                },
            ],
        };
        wm.evict_expired(10);
        assert_eq!(wm.entries.len(), 1);
        assert_eq!(wm.entries[0].key, "new");
    }

    // ── E1-T28: evict_expired keeps entries with no TTL ──
    #[test]
    fn evict_expired_keeps_no_ttl() {
        let mut wm = WorkingMemory {
            entries: vec![WorkingMemoryEntry {
                key: "permanent".into(),
                value: "stays".into(),
                written_at_tick: 0,
                ttl_ticks: None,
                relevance: 1.0,
            }],
        };
        wm.evict_expired(1000);
        assert_eq!(wm.entries.len(), 1);
    }

    // ── E1-T29: enforce_cap drops lowest relevance when over cap ──
    #[test]
    fn enforce_cap_drops_lowest_relevance() {
        let mut wm = WorkingMemory {
            entries: vec![
                WorkingMemoryEntry {
                    key: "low".into(),
                    value: "".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 0.1,
                },
                WorkingMemoryEntry {
                    key: "high".into(),
                    value: "".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 0.9,
                },
                WorkingMemoryEntry {
                    key: "mid".into(),
                    value: "".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 0.5,
                },
            ],
        };
        wm.enforce_cap(2);
        assert_eq!(wm.entries.len(), 2);
        let keys: Vec<&str> = wm.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"high"));
        assert!(keys.contains(&"mid"));
        assert!(!keys.contains(&"low"));
    }

    // ── E1-T30: enforce_cap no-op when under cap ──
    #[test]
    fn enforce_cap_noop_under_cap() {
        let mut wm = WorkingMemory {
            entries: vec![WorkingMemoryEntry {
                key: "only".into(),
                value: "one".into(),
                written_at_tick: 0,
                ttl_ticks: None,
                relevance: 1.0,
            }],
        };
        wm.enforce_cap(10);
        assert_eq!(wm.entries.len(), 1);
    }

    // ── E1-T31: from_legacy_string ──
    #[test]
    fn from_legacy_string() {
        let wm = WorkingMemory::from_legacy_string("Evaluating options".into());
        assert_eq!(wm.entries.len(), 1);
        assert_eq!(wm.entries[0].key, "legacy_context");
        assert_eq!(wm.entries[0].value, "Evaluating options");

        let empty = WorkingMemory::from_legacy_string(String::new());
        assert!(empty.is_empty());
    }

    // ── E1-T38: ts-rs generates valid TypeScript ──
    #[test]
    fn ts_rs_working_memory_types() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::default();
        let wm_decl = WorkingMemory::decl(&cfg);
        assert!(
            wm_decl.contains("WorkingMemory"),
            "WorkingMemory decl: {wm_decl}"
        );

        let entry_decl = WorkingMemoryEntry::decl(&cfg);
        assert!(
            entry_decl.contains("WorkingMemoryEntry"),
            "WorkingMemoryEntry decl: {entry_decl}"
        );
    }
}
