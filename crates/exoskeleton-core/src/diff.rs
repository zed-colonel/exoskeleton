//! Diff domain types for code mutation tracking (E9-S2, W-77).

use serde::{Deserialize, Serialize};

/// Structured content for a single-file CodeDiff artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct CodeDiffContent {
    /// Path of the modified file.
    pub file_path: String,
    /// The tool that produced this diff (`code.edit`, `code.write`, `code.apply_patch`).
    pub tool_name: String,
    /// Operation type inferred from the tool output.
    pub operation: CodeDiffOperation,
    /// Unified diff text extracted from the connector output.
    pub diff_text: String,
    /// Lines added.
    pub lines_added: i64,
    /// Lines removed.
    pub lines_removed: i64,
    /// SHA-256 of file content before the change, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_sha256: Option<String>,
    /// SHA-256 of file content after the change, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sha256: Option<String>,
}

/// Operation type for a code diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeDiffOperation {
    /// File was edited in place (`code.edit`).
    Edit,
    /// File was overwritten (`code.write` on existing file).
    Write,
    /// File was newly created (`code.write` on missing file).
    Create,
    /// A patch was applied (`code.apply_patch`).
    Patch,
}

/// Per-file entry in a TickDiffSummary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct FileDiffEntry {
    /// Path of the modified file.
    pub path: String,
    /// Lines added to this file.
    pub lines_added: i64,
    /// Lines removed from this file.
    pub lines_removed: i64,
    /// Operation type.
    pub operation: CodeDiffOperation,
}

/// Per-tick summary aggregating all file changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct TickDiffSummary {
    /// Tick number.
    pub tick_number: u64,
    /// Per-file change entries.
    pub files: Vec<FileDiffEntry>,
    /// Total lines added across all files.
    pub total_lines_added: i64,
    /// Total lines removed across all files.
    pub total_lines_removed: i64,
    /// Artifact IDs of the individual CodeDiff artifacts.
    pub diff_artifact_ids: Vec<String>,
    /// Whether this artifact is a summary artifact.
    pub is_summary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_diff_content_round_trip() {
        let original = CodeDiffContent {
            file_path: "src/main.rs".into(),
            tool_name: "code.edit".into(),
            operation: CodeDiffOperation::Edit,
            diff_text: "--- a/src/main.rs\n+++ b/src/main.rs\n".into(),
            lines_added: 5,
            lines_removed: 2,
            before_sha256: Some("before".into()),
            after_sha256: Some("after".into()),
        };

        let json = serde_json::to_string(&original).unwrap();
        let round_tripped: CodeDiffContent = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn tick_diff_summary_round_trip() {
        let original = TickDiffSummary {
            tick_number: 42,
            files: vec![
                FileDiffEntry {
                    path: "src/main.rs".into(),
                    lines_added: 3,
                    lines_removed: 1,
                    operation: CodeDiffOperation::Edit,
                },
                FileDiffEntry {
                    path: "tests/main.rs".into(),
                    lines_added: 8,
                    lines_removed: 0,
                    operation: CodeDiffOperation::Create,
                },
            ],
            total_lines_added: 11,
            total_lines_removed: 1,
            diff_artifact_ids: vec!["artifact-a".into(), "artifact-b".into()],
            is_summary: true,
        };

        let json = serde_json::to_string(&original).unwrap();
        let round_tripped: TickDiffSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn code_diff_operation_variants() {
        assert_eq!(
            serde_json::to_string(&CodeDiffOperation::Edit).unwrap(),
            "\"edit\""
        );
        assert_eq!(
            serde_json::to_string(&CodeDiffOperation::Write).unwrap(),
            "\"write\""
        );
        assert_eq!(
            serde_json::to_string(&CodeDiffOperation::Create).unwrap(),
            "\"create\""
        );
        assert_eq!(
            serde_json::to_string(&CodeDiffOperation::Patch).unwrap(),
            "\"patch\""
        );
    }
}
