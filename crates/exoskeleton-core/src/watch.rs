//! Watch primitives — persistent observation specifications.
//!
//! Watches are proposed by the Decide step, approved by Align, and checked
//! every tick (or every N ticks) during Perceive. Threshold watches monitor
//! internal metrics via IntrospectionService; poll watches dispatch Tool AQ
//! invocations with results arriving as events in the next Perceive cycle.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{PrincipalId, WatchId};
use crate::ExoError;

/// A persistent observation specification created by the Decide step.
///
/// Watches are proposed by the vessel, approved by the Align step, and
/// persisted in the WatchStore. They are checked every tick (or every N ticks
/// per their schedule) during the Perceive phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchDefinition {
    pub id: WatchId,
    pub name: String,
    pub description: String,
    pub watch_type: WatchType,
    pub schedule: WatchSchedule,
    pub status: WatchStatus,
    pub created_at_tick: u64,
    pub last_checked_tick: Option<u64>,
    pub trigger_count: u32,
    pub created_at: DateTime<Utc>,
}

/// What the watch monitors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchType {
    /// Monitor an internal metric (resolved during Perceive via IntrospectionService).
    /// No external I/O — synchronous store reads only.
    Threshold {
        metric: MetricKind,
        condition: WatchCondition,
    },
    /// Poll an external resource via connector invocation (dispatched through Tool AQ).
    /// Results arrive as events in the next Perceive cycle.
    Poll {
        connector: String,
        params: serde_json::Value,
        /// JSONPath-like key to extract a numeric value from the connector result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extract: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<WatchCondition>,
    },
}

/// Internal metrics that threshold watches can monitor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "metric", rename_all = "snake_case")]
pub enum MetricKind {
    /// Trust level for a specific principal (0.0–1.0).
    TrustLevel { principal_id: PrincipalId },
    /// Remaining budget in a named dimension (e.g., "frontier_tokens", "time_secs").
    BudgetRemaining { dimension: String },
    /// Number of consecutive tick failures (actions with non-success outcomes).
    ConsecutiveFailures,
    /// Duration of the most recent tick in milliseconds.
    TickDuration,
    /// Count of events of a given type in the last N ticks.
    EventCount {
        event_type: String,
        #[serde(default = "default_lookback")]
        lookback_ticks: u32,
    },
}

fn default_lookback() -> u32 {
    10
}

/// Condition that triggers a watch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WatchCondition {
    /// Triggers when the metric value exceeds the threshold.
    Above { value: f64 },
    /// Triggers when the metric value drops below the threshold.
    Below { value: f64 },
    /// Triggers when the metric value changes from the last check.
    Changed,
}

impl WatchCondition {
    /// Check whether the condition is satisfied given a current value
    /// and an optional previous value (for `Changed` conditions).
    pub fn is_triggered(&self, current: f64, previous: Option<f64>) -> bool {
        match self {
            WatchCondition::Above { value } => current > *value,
            WatchCondition::Below { value } => current < *value,
            WatchCondition::Changed => match previous {
                Some(prev) => (current - prev).abs() > f64::EPSILON,
                None => false, // No previous value → can't detect change
            },
        }
    }
}

/// How often the watch is checked.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchSchedule {
    /// Check every N ticks.
    EveryNTicks { n: u32 },
    /// Check once, then mark as Completed after triggering.
    Once,
}

/// Lifecycle state of a watch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchStatus {
    /// Watch is actively being checked.
    Active,
    /// Watch is paused (not checked, but not deleted).
    Paused,
    /// Once-type watch that has triggered — no longer checked.
    Completed,
}

/// A proposal from the Decide step to create a watch.
///
/// Does not yet have an ID or status — those are assigned after Align approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchProposal {
    pub name: String,
    pub description: String,
    pub watch_type: WatchType,
    pub schedule: WatchSchedule,
}

/// Maximum number of active watches per vessel (default, overridable in VesselConfig).
pub const DEFAULT_MAX_WATCHES: u32 = 20;

/// Persistent store for watch definitions.
///
/// Follows the same pattern as EventLedger and TickStore — trait in
/// exoskeleton-core, implementations in exoskeleton-host (SQLite) and
/// in-memory for tests.
pub trait WatchStore: Send + Sync {
    /// Persist a new watch definition.
    fn save(&self, watch: &WatchDefinition) -> Result<(), ExoError>;

    /// Get a watch by ID.
    fn get(&self, id: WatchId) -> Result<Option<WatchDefinition>, ExoError>;

    /// List all watches (any status).
    fn list(&self) -> Result<Vec<WatchDefinition>, ExoError>;

    /// List watches that are due to be checked on this tick.
    ///
    /// A watch is due when:
    /// - status is Active
    /// - schedule is Once and it hasn't triggered yet, OR
    /// - schedule is EveryNTicks(n) and (tick_number - last_checked_tick) >= n
    fn due_watches(&self, tick_number: u64) -> Result<Vec<WatchDefinition>, ExoError>;

    /// Update a watch's status.
    fn update_status(&self, id: WatchId, status: WatchStatus) -> Result<(), ExoError>;

    /// Record that a watch triggered: increment trigger_count, update last_checked_tick.
    /// For Once watches, also sets status to Completed.
    fn record_trigger(&self, id: WatchId, tick_number: u64) -> Result<(), ExoError>;

    /// Record that a watch was checked but did not trigger: update last_checked_tick.
    fn record_check(&self, id: WatchId, tick_number: u64) -> Result<(), ExoError>;

    /// Delete a watch by ID.
    fn delete(&self, id: WatchId) -> Result<(), ExoError>;

    /// Count active watches.
    fn active_count(&self) -> Result<u32, ExoError>;
}

/// In-memory WatchStore for unit tests.
pub struct InMemoryWatchStore {
    watches: Mutex<HashMap<WatchId, WatchDefinition>>,
}

impl InMemoryWatchStore {
    pub fn new() -> Self {
        Self {
            watches: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryWatchStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchStore for InMemoryWatchStore {
    fn save(&self, watch: &WatchDefinition) -> Result<(), ExoError> {
        self.watches.lock().unwrap().insert(watch.id, watch.clone());
        Ok(())
    }

    fn get(&self, id: WatchId) -> Result<Option<WatchDefinition>, ExoError> {
        Ok(self.watches.lock().unwrap().get(&id).cloned())
    }

    fn list(&self) -> Result<Vec<WatchDefinition>, ExoError> {
        Ok(self.watches.lock().unwrap().values().cloned().collect())
    }

    fn due_watches(&self, tick_number: u64) -> Result<Vec<WatchDefinition>, ExoError> {
        let watches = self.watches.lock().unwrap();
        let due = watches
            .values()
            .filter(|w| {
                if w.status != WatchStatus::Active {
                    return false;
                }
                match w.schedule {
                    WatchSchedule::Once => w.trigger_count == 0,
                    WatchSchedule::EveryNTicks { n } => {
                        let last = w.last_checked_tick.unwrap_or(w.created_at_tick);
                        tick_number.saturating_sub(last) >= n as u64
                    }
                }
            })
            .cloned()
            .collect();
        Ok(due)
    }

    fn update_status(&self, id: WatchId, status: WatchStatus) -> Result<(), ExoError> {
        match self.watches.lock().unwrap().get_mut(&id) {
            Some(w) => {
                w.status = status;
                Ok(())
            }
            None => Err(ExoError::Storage(format!("watch {id} not found"))),
        }
    }

    fn record_trigger(&self, id: WatchId, tick_number: u64) -> Result<(), ExoError> {
        match self.watches.lock().unwrap().get_mut(&id) {
            Some(w) => {
                w.trigger_count += 1;
                w.last_checked_tick = Some(tick_number);
                if w.schedule == WatchSchedule::Once {
                    w.status = WatchStatus::Completed;
                }
                Ok(())
            }
            None => Err(ExoError::Storage(format!("watch {id} not found"))),
        }
    }

    fn record_check(&self, id: WatchId, tick_number: u64) -> Result<(), ExoError> {
        match self.watches.lock().unwrap().get_mut(&id) {
            Some(w) => {
                w.last_checked_tick = Some(tick_number);
                Ok(())
            }
            None => Err(ExoError::Storage(format!("watch {id} not found"))),
        }
    }

    fn delete(&self, id: WatchId) -> Result<(), ExoError> {
        self.watches.lock().unwrap().remove(&id);
        Ok(())
    }

    fn active_count(&self) -> Result<u32, ExoError> {
        let count = self
            .watches
            .lock()
            .unwrap()
            .values()
            .filter(|w| w.status == WatchStatus::Active)
            .count() as u32;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── E5S2-T1: watch_definition_serde_roundtrip ──

    #[test]
    fn watch_definition_serde_roundtrip() {
        // Threshold watch
        let threshold = WatchDefinition {
            id: WatchId::new(),
            name: "trust-monitor".into(),
            description: "Monitor operator trust level".into(),
            watch_type: WatchType::Threshold {
                metric: MetricKind::TrustLevel {
                    principal_id: PrincipalId::new(),
                },
                condition: WatchCondition::Below { value: 0.3 },
            },
            schedule: WatchSchedule::EveryNTicks { n: 5 },
            status: WatchStatus::Active,
            created_at_tick: 10,
            last_checked_tick: Some(15),
            trigger_count: 1,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&threshold).unwrap();
        let parsed: WatchDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(threshold, parsed);

        // Poll watch
        let poll = WatchDefinition {
            id: WatchId::new(),
            name: "api-health".into(),
            description: "Poll API health endpoint".into(),
            watch_type: WatchType::Poll {
                connector: "http.request".into(),
                params: serde_json::json!({"url": "https://example.com/health"}),
                extract: Some("status".into()),
                condition: Some(WatchCondition::Above { value: 500.0 }),
            },
            schedule: WatchSchedule::Once,
            status: WatchStatus::Active,
            created_at_tick: 1,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&poll).unwrap();
        let parsed: WatchDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(poll, parsed);

        // All WatchCondition variants
        let changed = WatchCondition::Changed;
        let json = serde_json::to_string(&changed).unwrap();
        let parsed: WatchCondition = serde_json::from_str(&json).unwrap();
        assert_eq!(changed, parsed);

        // All WatchStatus variants
        for status in [
            WatchStatus::Active,
            WatchStatus::Paused,
            WatchStatus::Completed,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: WatchStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }

        // All MetricKind variants
        let metrics = vec![
            MetricKind::TrustLevel {
                principal_id: PrincipalId::new(),
            },
            MetricKind::BudgetRemaining {
                dimension: "frontier_tokens".into(),
            },
            MetricKind::ConsecutiveFailures,
            MetricKind::TickDuration,
            MetricKind::EventCount {
                event_type: "action_executed".into(),
                lookback_ticks: 10,
            },
        ];
        for metric in &metrics {
            let json = serde_json::to_string(metric).unwrap();
            let parsed: MetricKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*metric, parsed);
        }

        // All WatchSchedule variants
        for schedule in [WatchSchedule::EveryNTicks { n: 3 }, WatchSchedule::Once] {
            let json = serde_json::to_string(&schedule).unwrap();
            let parsed: WatchSchedule = serde_json::from_str(&json).unwrap();
            assert_eq!(schedule, parsed);
        }
    }

    #[test]
    fn watch_condition_is_triggered() {
        // Above
        assert!(WatchCondition::Above { value: 0.5 }.is_triggered(0.6, None));
        assert!(!WatchCondition::Above { value: 0.5 }.is_triggered(0.4, None));

        // Below
        assert!(WatchCondition::Below { value: 0.5 }.is_triggered(0.3, None));
        assert!(!WatchCondition::Below { value: 0.5 }.is_triggered(0.7, None));

        // Changed
        assert!(WatchCondition::Changed.is_triggered(1.0, Some(0.5)));
        assert!(!WatchCondition::Changed.is_triggered(1.0, Some(1.0)));
        assert!(!WatchCondition::Changed.is_triggered(1.0, None));
    }

    #[test]
    fn watch_store_in_memory_crud() {
        let store = InMemoryWatchStore::new();

        let watch = WatchDefinition {
            id: WatchId::new(),
            name: "test-watch".into(),
            description: "A test watch".into(),
            watch_type: WatchType::Threshold {
                metric: MetricKind::ConsecutiveFailures,
                condition: WatchCondition::Above { value: 3.0 },
            },
            schedule: WatchSchedule::EveryNTicks { n: 1 },
            status: WatchStatus::Active,
            created_at_tick: 0,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        };

        store.save(&watch).unwrap();
        assert_eq!(store.get(watch.id).unwrap().unwrap().name, "test-watch");
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.active_count().unwrap(), 1);

        store.update_status(watch.id, WatchStatus::Paused).unwrap();
        assert_eq!(
            store.get(watch.id).unwrap().unwrap().status,
            WatchStatus::Paused
        );
        assert_eq!(store.active_count().unwrap(), 0);

        store.delete(watch.id).unwrap();
        assert!(store.get(watch.id).unwrap().is_none());
    }

    #[test]
    fn watch_store_due_watches_logic() {
        let store = InMemoryWatchStore::new();

        // EveryNTicks(5), created at tick 0
        let w1 = WatchDefinition {
            id: WatchId::new(),
            name: "every-5".into(),
            description: "".into(),
            watch_type: WatchType::Threshold {
                metric: MetricKind::TickDuration,
                condition: WatchCondition::Above { value: 1000.0 },
            },
            schedule: WatchSchedule::EveryNTicks { n: 5 },
            status: WatchStatus::Active,
            created_at_tick: 0,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        };

        // Once watch, not yet triggered
        let w2 = WatchDefinition {
            id: WatchId::new(),
            name: "once-watch".into(),
            description: "".into(),
            watch_type: WatchType::Threshold {
                metric: MetricKind::ConsecutiveFailures,
                condition: WatchCondition::Above { value: 5.0 },
            },
            schedule: WatchSchedule::Once,
            status: WatchStatus::Active,
            created_at_tick: 0,
            last_checked_tick: None,
            trigger_count: 0,
            created_at: Utc::now(),
        };

        store.save(&w1).unwrap();
        store.save(&w2).unwrap();

        // At tick 3: EveryNTicks(5) not due (3-0 < 5), Once is due
        let due = store.due_watches(3).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "once-watch");

        // At tick 5: both due
        let due = store.due_watches(5).unwrap();
        assert_eq!(due.len(), 2);

        // Record trigger on once-watch → should complete
        store.record_trigger(w2.id, 5).unwrap();
        let w2_after = store.get(w2.id).unwrap().unwrap();
        assert_eq!(w2_after.status, WatchStatus::Completed);
        assert_eq!(w2_after.trigger_count, 1);

        // Once-watch no longer due
        let due = store.due_watches(10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "every-5");

        // Record check on every-5 at tick 5
        store.record_check(w1.id, 5).unwrap();
        // At tick 9: not due (9-5 < 5)
        let due = store.due_watches(9).unwrap();
        assert_eq!(due.len(), 0);
        // At tick 10: due again (10-5 >= 5)
        let due = store.due_watches(10).unwrap();
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn event_count_lookback_default() {
        let json = r#"{"metric":"event_count","event_type":"action_executed"}"#;
        let parsed: MetricKind = serde_json::from_str(json).unwrap();
        match parsed {
            MetricKind::EventCount { lookback_ticks, .. } => {
                assert_eq!(lookback_ticks, 10);
            }
            _ => panic!("expected EventCount"),
        }
    }
}
