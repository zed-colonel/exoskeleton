//! Structured plan model for Exoskeleton cognitive planning (W-11).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::PlanTaskId;

/// A structured plan with objective and tracked tasks.
///
/// Replaces the freeform `Option<String>` plan in StateSnapshot.
/// Uses a flat task list with dependency edges — NOT a hierarchical tree.
/// This design supports DAGs (task depends on multiple predecessors),
/// is forward-compatible with multi-step tool chains (W-53-55),
/// and is easier for LLMs to manipulate than tree structures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct Plan {
    /// High-level objective this plan achieves.
    pub objective: String,
    /// Ordered list of tasks in this plan.
    pub tasks: Vec<PlanTask>,
    /// When this plan was last updated.
    pub updated_at: DateTime<Utc>,
}

/// A single task within a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct PlanTask {
    /// Unique identity of this task within the plan.
    pub id: PlanTaskId,
    /// Human-readable description of the task.
    pub description: String,
    /// Current status.
    pub status: PlanTaskStatus,
    /// IDs of tasks that must complete before this one can start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<PlanTaskId>,
    /// Optional hint about which tool to use (suggestion, not constraint).
    /// Forward-compatible with W-57 (WASM plugins register new tool names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_hint: Option<String>,
    /// Extensible key-value metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// Status of a plan task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PlanTaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
    Skipped,
}

/// Instruction for updating the plan (emitted by Decide step).
///
/// Two modes: Replace (initial planning, major pivots) or Patch (incremental).
/// Local models will likely always use Replace; frontier models can use Patch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanUpdate {
    /// Replace the entire plan.
    Replace { plan: Plan },
    /// Apply a list of incremental operations.
    Patch { operations: Vec<PlanOp> },
}

/// A single incremental operation on a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlanOp {
    AddTask {
        task: PlanTask,
    },
    UpdateStatus {
        task_id: PlanTaskId,
        new_status: PlanTaskStatus,
    },
    RemoveTask {
        task_id: PlanTaskId,
    },
    UpdateObjective {
        objective: String,
    },
}

impl Plan {
    /// Apply a PlanUpdate, returning the updated plan.
    pub fn apply_update(self, update: PlanUpdate) -> Self {
        match update {
            PlanUpdate::Replace { plan } => plan,
            PlanUpdate::Patch { operations } => {
                let mut plan = self;
                for op in operations {
                    plan.apply_op(op);
                }
                plan.updated_at = Utc::now();
                plan
            }
        }
    }

    /// Apply a single operation. Nonexistent task_id is a no-op (logged).
    pub fn apply_op(&mut self, op: PlanOp) {
        match op {
            PlanOp::AddTask { task } => {
                self.tasks.push(task);
            }
            PlanOp::UpdateStatus {
                task_id,
                new_status,
            } => {
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = new_status;
                }
                // Nonexistent task_id is a silent no-op; callers log if needed
            }
            PlanOp::RemoveTask { task_id } => {
                self.tasks.retain(|t| t.id != task_id);
            }
            PlanOp::UpdateObjective { objective } => {
                self.objective = objective;
            }
        }
    }

    /// Create a plan from a legacy string (serde migration helper).
    pub fn from_legacy_string(text: String) -> Self {
        Self {
            objective: text,
            tasks: Vec::new(),
            updated_at: Utc::now(),
        }
    }

    /// Count tasks by status.
    pub fn task_counts(&self) -> HashMap<PlanTaskStatus, usize> {
        let mut counts = HashMap::new();
        for task in &self.tasks {
            *counts.entry(task.status).or_insert(0) += 1;
        }
        counts
    }
}

/// Custom deserializer: handles legacy string and new Plan struct.
///
/// - `"plan": "some text"` -> `Some(Plan::from_legacy_string("some text"))`
/// - `"plan": {"objective": "...", ...}` -> `Some(Plan { ... })`
/// - `"plan": null` or field absent -> `None`
pub fn deserialize_plan_compat<'de, D>(deserializer: D) -> Result<Option<Plan>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Plan::from_legacy_string(s)))
            }
        }
        Some(v @ serde_json::Value::Object(_)) => {
            let plan: Plan = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
            Ok(Some(plan))
        }
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected string, object, or null for plan, got: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::PlanTaskId;

    fn make_task(desc: &str, status: PlanTaskStatus) -> PlanTask {
        PlanTask {
            id: PlanTaskId::new(),
            description: desc.into(),
            status,
            depends_on: Vec::new(),
            tool_hint: None,
            metadata: HashMap::new(),
        }
    }

    fn make_plan() -> Plan {
        Plan {
            objective: "Test objective".into(),
            tasks: vec![
                make_task("Task A", PlanTaskStatus::Completed),
                make_task("Task B", PlanTaskStatus::InProgress),
                make_task("Task C", PlanTaskStatus::Pending),
            ],
            updated_at: Utc::now(),
        }
    }

    // ── E1-T1: Plan JSON roundtrip ──
    #[test]
    fn plan_json_roundtrip_full() {
        let dep_id = PlanTaskId::new();
        let plan = Plan {
            objective: "Achieve goal X".into(),
            tasks: vec![
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "First task".into(),
                    status: PlanTaskStatus::Completed,
                    depends_on: vec![],
                    tool_hint: Some("fs.write".into()),
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("key".into(), "value".into());
                        m
                    },
                },
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Second task".into(),
                    status: PlanTaskStatus::Pending,
                    depends_on: vec![dep_id],
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
            ],
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    // ── E1-T2: PlanTaskStatus all 6 variants roundtrip ──
    #[test]
    fn plan_task_status_all_variants_roundtrip() {
        let variants = [
            PlanTaskStatus::Pending,
            PlanTaskStatus::InProgress,
            PlanTaskStatus::Completed,
            PlanTaskStatus::Failed,
            PlanTaskStatus::Blocked,
            PlanTaskStatus::Skipped,
        ];
        for status in &variants {
            let json = serde_json::to_string(status).unwrap();
            let parsed: PlanTaskStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(*status, parsed);
        }
        // Verify snake_case serialization
        assert_eq!(
            serde_json::to_string(&PlanTaskStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
    }

    // ── E1-T3: PlanUpdate::Replace roundtrip ──
    #[test]
    fn plan_update_replace_roundtrip() {
        let update = PlanUpdate::Replace { plan: make_plan() };
        let json = serde_json::to_string(&update).unwrap();
        let parsed: PlanUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, parsed);
        // Verify tagged enum format
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "replace");
    }

    // ── E1-T4: PlanUpdate::Patch with all 4 PlanOp variants roundtrip ──
    #[test]
    fn plan_update_patch_all_ops_roundtrip() {
        let task_id = PlanTaskId::new();
        let update = PlanUpdate::Patch {
            operations: vec![
                PlanOp::AddTask {
                    task: make_task("New task", PlanTaskStatus::Pending),
                },
                PlanOp::UpdateStatus {
                    task_id,
                    new_status: PlanTaskStatus::Completed,
                },
                PlanOp::RemoveTask { task_id },
                PlanOp::UpdateObjective {
                    objective: "New objective".into(),
                },
            ],
        };
        let json = serde_json::to_string(&update).unwrap();
        let parsed: PlanUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, parsed);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "patch");
    }

    // ── E1-T5: apply_update(Replace) replaces entire plan ──
    #[test]
    fn apply_update_replace() {
        let original = make_plan();
        let replacement = Plan {
            objective: "Completely new".into(),
            tasks: vec![make_task("Only task", PlanTaskStatus::Pending)],
            updated_at: Utc::now(),
        };
        let result = original.apply_update(PlanUpdate::Replace {
            plan: replacement.clone(),
        });
        assert_eq!(result.objective, "Completely new");
        assert_eq!(result.tasks.len(), 1);
    }

    // ── E1-T6: apply_update(Patch) with all ops ──
    #[test]
    fn apply_update_patch_all_ops() {
        let plan = make_plan();
        let task_b_id = plan.tasks[1].id;
        let new_task = make_task("Task D", PlanTaskStatus::Pending);

        let result = plan.clone().apply_update(PlanUpdate::Patch {
            operations: vec![
                PlanOp::AddTask {
                    task: new_task.clone(),
                },
                PlanOp::UpdateStatus {
                    task_id: task_b_id,
                    new_status: PlanTaskStatus::Completed,
                },
                PlanOp::RemoveTask {
                    task_id: plan.tasks[0].id,
                },
                PlanOp::UpdateObjective {
                    objective: "Updated".into(),
                },
            ],
        });

        assert_eq!(result.objective, "Updated");
        // Original had 3 tasks. Added 1 = 4, removed 1 = 3.
        assert_eq!(result.tasks.len(), 3);
        // Task B should now be Completed
        let task_b = result.tasks.iter().find(|t| t.id == task_b_id).unwrap();
        assert_eq!(task_b.status, PlanTaskStatus::Completed);
    }

    // ── E1-T7: apply_op(UpdateStatus) with nonexistent task_id is no-op ──
    #[test]
    fn apply_op_update_status_nonexistent_is_noop() {
        let mut plan = make_plan();
        let original_len = plan.tasks.len();
        let bogus_id = PlanTaskId::new();
        plan.apply_op(PlanOp::UpdateStatus {
            task_id: bogus_id,
            new_status: PlanTaskStatus::Failed,
        });
        assert_eq!(plan.tasks.len(), original_len);
        // No task should have Failed status
        assert!(plan
            .tasks
            .iter()
            .all(|t| t.status != PlanTaskStatus::Failed));
    }

    // ── E1-T8: from_legacy_string creates plan with objective, empty tasks ──
    #[test]
    fn from_legacy_string() {
        let plan = Plan::from_legacy_string("Execute plan A".into());
        assert_eq!(plan.objective, "Execute plan A");
        assert!(plan.tasks.is_empty());
    }

    // ── E1-T9: task_counts returns correct tallies ──
    #[test]
    fn task_counts() {
        let plan = make_plan();
        let counts = plan.task_counts();
        assert_eq!(counts[&PlanTaskStatus::Completed], 1);
        assert_eq!(counts[&PlanTaskStatus::InProgress], 1);
        assert_eq!(counts[&PlanTaskStatus::Pending], 1);
        assert_eq!(counts.get(&PlanTaskStatus::Failed), None);
    }

    // ── E1-T19: PlanTaskId via define_id! ──
    #[test]
    fn plan_task_id_basics() {
        let id = PlanTaskId::new();
        let s = id.to_string();
        let parsed: PlanTaskId = s.parse().unwrap();
        assert_eq!(id, parsed);

        let uuid = uuid::Uuid::new_v4();
        let from_uuid = PlanTaskId::from(uuid);
        assert_eq!(*from_uuid.as_ref(), uuid);

        // Hash consistency
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h1 = DefaultHasher::new();
        id.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        id.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    // ── E1-T20: ts-rs generates valid TypeScript ──
    #[test]
    fn ts_rs_plan_types() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::default();
        let plan_decl = Plan::decl(&cfg);
        assert!(plan_decl.contains("Plan"), "Plan decl: {plan_decl}");

        let task_decl = PlanTask::decl(&cfg);
        assert!(task_decl.contains("PlanTask"), "PlanTask decl: {task_decl}");

        let status_decl = PlanTaskStatus::decl(&cfg);
        assert!(
            status_decl.contains("PlanTaskStatus"),
            "PlanTaskStatus decl: {status_decl}"
        );
    }
}
