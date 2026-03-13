//! Reflect step — heuristic evaluation of tick outcomes.

use exoskeleton_core::tick::ActionOutcome;

use super::types::{ActResult, ReflectionResult};

/// Execute the Reflect step: evaluate tick outcomes.
///
/// Sprint 5: heuristic evaluation. No LLM call.
pub fn reflect(act_result: &ActResult) -> ReflectionResult {
    let total = act_result.executions.len();

    if total == 0 {
        return ReflectionResult {
            action_success_rate: f64::NAN,
            observations: vec!["No actions taken".into()],
            concerns: vec![],
        };
    }

    let successes = act_result
        .executions
        .iter()
        .filter(|e| e.record.outcome == ActionOutcome::Success)
        .count();
    let failures = total - successes;
    let rate = successes as f64 / total as f64;

    let mut observations = vec![format!(
        "Executed {total} actions: {successes} succeeded, {failures} failed"
    )];

    for exec in &act_result.executions {
        if exec.result.is_err() {
            observations.push(format!(
                "Failed action: {}: {}",
                exec.action.tool_name,
                exec.result.as_ref().unwrap_err()
            ));
        }
    }

    let mut concerns = vec![];
    if rate == 0.0 {
        concerns.push("All actions failed — possible misconfiguration".into());
    } else if rate < 0.5 {
        concerns.push("More than half of actions failed".into());
    }

    ReflectionResult {
        action_success_rate: rate,
        observations,
        concerns,
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::tick::ActionRecord;

    use super::*;
    use crate::kernel::types::{ActionExecution, PlannedAction};

    fn success_execution(name: &str) -> ActionExecution {
        ActionExecution {
            action: PlannedAction {
                tool_name: name.into(),
                params: serde_json::json!({}),
                rationale: "test".into(),
            },
            result: Ok(serde_json::json!({"ok": true})),
            record: ActionRecord {
                action_type: name.into(),
                target: "test".into(),
                receipt_ref: None,
                outcome: ActionOutcome::Success,
            },
        }
    }

    fn failure_execution(name: &str, error: &str) -> ActionExecution {
        ActionExecution {
            action: PlannedAction {
                tool_name: name.into(),
                params: serde_json::json!({}),
                rationale: "test".into(),
            },
            result: Err(error.into()),
            record: ActionRecord {
                action_type: name.into(),
                target: "test".into(),
                receipt_ref: None,
                outcome: ActionOutcome::Failure,
            },
        }
    }

    #[test]
    fn reflect_all_success() {
        let result = reflect(&ActResult {
            executions: vec![
                success_execution("a"),
                success_execution("b"),
                success_execution("c"),
            ],
        });
        assert_eq!(result.action_success_rate, 1.0);
        assert!(result.concerns.is_empty());
    }

    #[test]
    fn reflect_mixed_results() {
        let result = reflect(&ActResult {
            executions: vec![
                success_execution("a"),
                success_execution("b"),
                failure_execution("c", "timeout"),
            ],
        });
        assert!((result.action_success_rate - 2.0 / 3.0).abs() < 0.01);
        assert!(result.concerns.is_empty()); // rate > 0.5
    }

    #[test]
    fn reflect_all_failures() {
        let result = reflect(&ActResult {
            executions: vec![
                failure_execution("a", "err1"),
                failure_execution("b", "err2"),
                failure_execution("c", "err3"),
            ],
        });
        assert_eq!(result.action_success_rate, 0.0);
        assert!(result
            .concerns
            .iter()
            .any(|c| c.contains("All actions failed")));
    }

    #[test]
    fn reflect_no_actions() {
        let result = reflect(&ActResult { executions: vec![] });
        assert!(result.action_success_rate.is_nan());
        assert!(result
            .observations
            .iter()
            .any(|o| o.contains("No actions taken")));
    }

    #[test]
    fn reflect_generates_observation_summary() {
        let result = reflect(&ActResult {
            executions: vec![
                success_execution("delay"),
                failure_execution("fs.write", "denied"),
            ],
        });
        assert!(result.observations[0].contains("2 actions"));
        assert!(result.observations[0].contains("1 succeeded"));
        assert!(result.observations[0].contains("1 failed"));
    }
}
