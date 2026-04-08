//! Inner-loop context windowing (E10-S1, W-96).

use serde_json::Value;

use super::ActionExecution;

const FULL_OUTPUT_LIMIT: usize = 2000;

pub fn format_windowed_results(executions: &[ActionExecution], context_window_size: u32) -> String {
    if executions.is_empty() {
        return "No tool results yet.".into();
    }

    let recent_count = context_window_size as usize;
    let recent_start = executions.len().saturating_sub(recent_count);
    let mut output = String::from("## Tool Results\n\n");

    if recent_start > 0 {
        output.push_str("### Earlier Steps\n");
        for (index, execution) in executions.iter().take(recent_start).enumerate() {
            output.push_str(&summarize_execution(index + 1, execution));
            output.push('\n');
        }
        output.push('\n');
    }

    for (index, execution) in executions.iter().enumerate().skip(recent_start) {
        let step = index + 1;
        let status = if execution.result.is_ok() {
            "SUCCESS"
        } else {
            "FAILED"
        };
        output.push_str(&format!(
            "### Step {step}: {} [{status}]\n",
            execution.action.tool_name
        ));
        match &execution.result {
            Ok(value) => output.push_str(&format!("Output: {}\n\n", truncate_json(value))),
            Err(error) => output.push_str(&format!("Error: {error}\n\n")),
        }
    }

    output
}

fn summarize_execution(step: usize, execution: &ActionExecution) -> String {
    let tool = execution.action.tool_name.as_str();
    match &execution.result {
        Ok(value) => match tool {
            "code.read" => summarize_code_read(step, execution, value),
            "code.grep" => summarize_code_grep(step, execution, value),
            "code.edit" | "code.write" | "code.apply_patch" => {
                summarize_code_mutation(step, execution, value)
            }
            "code.glob" => summarize_code_glob(step, value),
            "code.ls" => summarize_code_ls(step, value),
            _ => format!("Step {step}: {tool} -> success"),
        },
        Err(error) => format!("Step {step}: {tool} -> FAILED: {error}"),
    }
}

fn summarize_code_read(step: usize, execution: &ActionExecution, value: &Value) -> String {
    let file_path = execution.action.params["file_path"]
        .as_str()
        .or_else(|| value["file_path"].as_str())
        .unwrap_or("<unknown>");
    let start = value["start_line"].as_u64().unwrap_or(1);
    let end = value["end_line"].as_u64().unwrap_or(start);
    let lines_read = if end >= start { end - start + 1 } else { 0 };
    format!("Step {step}: code.read {file_path}:{start}-{end} -> {lines_read} lines (success)")
}

fn summarize_code_grep(step: usize, execution: &ActionExecution, value: &Value) -> String {
    let pattern = execution.action.params["pattern"]
        .as_str()
        .unwrap_or("<pattern>");
    let matches = value["total_matches"].as_u64().unwrap_or(0);
    let files = value["files_searched"].as_u64().unwrap_or(0);
    format!("Step {step}: code.grep '{pattern}' -> {matches} matches in {files} files (success)")
}

fn summarize_code_mutation(step: usize, execution: &ActionExecution, value: &Value) -> String {
    let file_path = value["file_path"]
        .as_str()
        .or_else(|| execution.action.params["file_path"].as_str())
        .unwrap_or("<unknown>");
    let added = value["diff"]["lines_added"].as_i64().unwrap_or(0);
    let removed = value["diff"]["lines_removed"].as_i64().unwrap_or(0);
    format!(
        "Step {step}: {} {file_path} -> +{added}/-{removed} lines (success)",
        execution.action.tool_name
    )
}

fn summarize_code_glob(step: usize, value: &Value) -> String {
    let matches = value["total_matches"].as_u64().unwrap_or(0);
    format!("Step {step}: code.glob -> {matches} files (success)")
}

fn summarize_code_ls(step: usize, value: &Value) -> String {
    let entries = value["total_entries"].as_u64().unwrap_or(0);
    format!("Step {step}: code.ls -> {entries} entries (success)")
}

fn truncate_json(value: &Value) -> String {
    let json = value.to_string();
    if json.len() > FULL_OUTPUT_LIMIT {
        format!("{}...", &json[..FULL_OUTPUT_LIMIT - 3])
    } else {
        json
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::tick::{ActionOutcome, ActionRecord};
    use serde_json::json;

    use super::*;
    use crate::kernel::types::{ActionExecution, PlannedAction};

    fn execution(tool: &str, params: Value, result: Result<Value, &str>) -> ActionExecution {
        let success = result.is_ok();
        ActionExecution {
            action: PlannedAction {
                call_id: format!("call_{tool}"),
                tool_name: tool.into(),
                params,
                rationale: "test".into(),
                plan_task_id: None,
            },
            result: result.map_err(|err| err.to_string()),
            record: ActionRecord {
                action_type: tool.into(),
                target: "test-target".into(),
                receipt_ref: None,
                outcome: if success {
                    ActionOutcome::Success
                } else {
                    ActionOutcome::Failure
                },
            },
            tool_result: exoskeleton_core::llm::ContentBlock::ToolResult {
                tool_use_id: format!("call_{tool}"),
                content: "{}".into(),
                is_error: !success,
            },
            pending_question: false,
            code_diff: None,
        }
    }

    #[test]
    fn format_windowed_results_summarizes_older_steps() {
        let executions = vec![
            execution(
                "code.read",
                json!({"file_path": "src/main.rs"}),
                Ok(json!({"start_line": 1, "end_line": 50, "total_lines": 120})),
            ),
            execution(
                "code.grep",
                json!({"pattern": "fn parse"}),
                Ok(json!({"total_matches": 4, "files_searched": 2})),
            ),
            execution(
                "code.edit",
                json!({"file_path": "src/lib.rs"}),
                Ok(
                    json!({"file_path": "src/lib.rs", "diff": {"lines_added": 3, "lines_removed": 1}}),
                ),
            ),
            execution(
                "shell.exec",
                json!({"command": "cargo test"}),
                Err("tests failed"),
            ),
        ];

        let formatted = format_windowed_results(&executions, 2);
        assert!(formatted.contains("### Earlier Steps"));
        assert!(formatted.contains("Step 1: code.read src/main.rs:1-50 -> 50 lines (success)"));
        assert!(
            formatted.contains("Step 2: code.grep 'fn parse' -> 4 matches in 2 files (success)")
        );
        assert!(formatted.contains("### Step 3: code.edit [SUCCESS]"));
        assert!(formatted.contains("### Step 4: shell.exec [FAILED]"));
    }

    #[test]
    fn format_windowed_results_truncates_full_output() {
        let long = "x".repeat(2500);
        let executions = vec![execution(
            "code.read",
            json!({}),
            Ok(json!({"content": long})),
        )];
        let formatted = format_windowed_results(&executions, 3);
        assert!(formatted.contains("Output: "));
        assert!(formatted.contains("..."));
    }

    #[test]
    fn format_windowed_results_empty() {
        assert_eq!(format_windowed_results(&[], 3), "No tool results yet.");
    }

    #[test]
    fn format_windowed_results_all_within_window_shows_full_output_only() {
        let executions = vec![
            execution(
                "code.read",
                json!({"file_path": "src/main.rs"}),
                Ok(json!({"content": "line 1", "start_line": 1, "end_line": 1, "total_lines": 10})),
            ),
            execution(
                "code.grep",
                json!({"pattern": "main"}),
                Ok(
                    json!({"content": "src/main.rs:1:fn main()", "total_matches": 1, "files_searched": 1}),
                ),
            ),
        ];

        let formatted = format_windowed_results(&executions, 3);

        assert!(!formatted.contains("### Earlier Steps"));
        assert!(formatted.contains("### Step 1: code.read [SUCCESS]"));
        assert!(formatted.contains("### Step 2: code.grep [SUCCESS]"));
        assert!(formatted.contains("\"content\":\"line 1\""));
        assert!(formatted.contains("\"content\":\"src/main.rs:1:fn main()\""));
    }
}
