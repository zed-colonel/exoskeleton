//! Per-tick diff accumulator for CodeDiff artifact assembly (E9-S2, W-77).

use std::sync::Arc;

use exoskeleton_core::{
    Artifact, ArtifactId, ArtifactKind, ArtifactStore, CodeDiffContent, DiffSummary, FileDiffEntry,
    TickDiffSummary,
};

/// Accumulates CodeDiff artifacts during a tick and assembles a TickDiffSummary.
#[derive(Debug)]
pub struct DiffTracker {
    entries: Vec<(ArtifactId, CodeDiffContent)>,
    tick_number: u64,
}

impl DiffTracker {
    /// Create a new tracker for the given tick.
    pub fn new(tick_number: u64) -> Self {
        Self {
            entries: Vec::new(),
            tick_number,
        }
    }

    /// Record a CodeDiff artifact produced during the tick.
    pub fn record(&mut self, artifact_id: ArtifactId, content: CodeDiffContent) {
        self.entries.push((artifact_id, content));
    }

    /// Whether any diffs were recorded.
    pub fn has_diffs(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Number of recorded diffs.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tracker has no recorded diffs.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build a DiffSummary for LiveEvent population.
    pub fn build_event_summary(&self) -> Option<DiffSummary> {
        if self.is_empty() {
            return None;
        }

        let total_added: i64 = self.entries.iter().map(|(_, c)| c.lines_added).sum();
        let total_removed: i64 = self.entries.iter().map(|(_, c)| c.lines_removed).sum();

        Some(DiffSummary {
            files_modified: self.entries.len() as u32,
            lines_added: total_added,
            lines_removed: total_removed,
            net_delta: total_added - total_removed,
            files: self
                .entries
                .iter()
                .map(|(_, c)| FileDiffEntry {
                    path: c.file_path.clone(),
                    lines_added: c.lines_added,
                    lines_removed: c.lines_removed,
                    operation: c.operation,
                })
                .collect(),
        })
    }

    /// Assemble the TickDiffSummary, store it as an artifact, and return both.
    pub fn finalize(
        self,
        artifact_store: &Arc<dyn ArtifactStore>,
    ) -> Option<(ArtifactId, TickDiffSummary)> {
        if self.is_empty() {
            return None;
        }

        let files: Vec<FileDiffEntry> = self
            .entries
            .iter()
            .map(|(_, c)| FileDiffEntry {
                path: c.file_path.clone(),
                lines_added: c.lines_added,
                lines_removed: c.lines_removed,
                operation: c.operation,
            })
            .collect();

        let total_added: i64 = self.entries.iter().map(|(_, c)| c.lines_added).sum();
        let total_removed: i64 = self.entries.iter().map(|(_, c)| c.lines_removed).sum();
        let artifact_ids: Vec<String> = self.entries.iter().map(|(id, _)| id.to_string()).collect();

        let summary = TickDiffSummary {
            tick_number: self.tick_number,
            files,
            total_lines_added: total_added,
            total_lines_removed: total_removed,
            diff_artifact_ids: artifact_ids,
            is_summary: true,
        };

        let artifact = Artifact::from_json(ArtifactKind::CodeDiff, &summary).ok()?;
        let id = artifact_store.put(&artifact).ok()?;

        Some((id, summary))
    }

    /// Format a human-readable diff summary for Reflect prompt inclusion.
    pub fn format_for_reflect(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let total_added: i64 = self.entries.iter().map(|(_, c)| c.lines_added).sum();
        let total_removed: i64 = self.entries.iter().map(|(_, c)| c.lines_removed).sum();

        let mut msg = format!(
            "\n## File Changes This Tick\n\nModified {} file(s) (+{} lines, -{} lines):\n",
            self.entries.len(),
            total_added,
            total_removed,
        );
        for (_, content) in &self.entries {
            msg.push_str(&format!(
                "- {}: {:?} (+{}, -{})\n",
                content.file_path, content.operation, content.lines_added, content.lines_removed,
            ));
        }
        msg.push_str(
            "\nReview: Are these changes correct and complete? Did the agent miss any files?\n",
        );
        msg
    }
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{ArtifactStore, CodeDiffOperation};

    use super::*;
    use crate::storage::StorageManager;

    fn sample_content(
        path: &str,
        operation: CodeDiffOperation,
        lines_added: i64,
        lines_removed: i64,
    ) -> CodeDiffContent {
        CodeDiffContent {
            file_path: path.into(),
            tool_name: "code.edit".into(),
            operation,
            diff_text: "--- a/file\n+++ b/file\n".into(),
            lines_added,
            lines_removed,
            before_sha256: None,
            after_sha256: None,
        }
    }

    #[test]
    fn diff_tracker_empty_returns_none() {
        let tracker = DiffTracker::new(7);
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        assert!(tracker.build_event_summary().is_none());
        assert!(tracker.finalize(&artifact_store).is_none());
    }

    #[test]
    fn diff_tracker_single_entry() {
        let mut tracker = DiffTracker::new(7);
        tracker.record(
            ArtifactId::from_content(b"diff-a"),
            sample_content("src/main.rs", CodeDiffOperation::Edit, 3, 1),
        );

        let summary = tracker.build_event_summary().unwrap();
        assert_eq!(summary.files_modified, 1);
        assert_eq!(summary.lines_added, 3);
        assert_eq!(summary.lines_removed, 1);
        assert_eq!(summary.net_delta, 2);
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].path, "src/main.rs");
    }

    #[test]
    fn diff_tracker_multiple_entries_aggregates() {
        let mut tracker = DiffTracker::new(8);
        tracker.record(
            ArtifactId::from_content(b"diff-a"),
            sample_content("src/main.rs", CodeDiffOperation::Edit, 3, 1),
        );
        tracker.record(
            ArtifactId::from_content(b"diff-b"),
            sample_content("tests/main.rs", CodeDiffOperation::Create, 7, 0),
        );

        let summary = tracker.build_event_summary().unwrap();
        assert_eq!(summary.files_modified, 2);
        assert_eq!(summary.lines_added, 10);
        assert_eq!(summary.lines_removed, 1);
        assert_eq!(summary.net_delta, 9);
        assert_eq!(summary.files.len(), 2);
    }

    #[test]
    fn diff_tracker_finalize_stores_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageManager::open(dir.path()).unwrap();
        let artifact_store: Arc<dyn ArtifactStore> = storage.artifact_store().clone();

        let mut tracker = DiffTracker::new(9);
        tracker.record(
            ArtifactId::from_content(b"diff-a"),
            sample_content("src/main.rs", CodeDiffOperation::Edit, 3, 1),
        );

        let (artifact_id, summary) = tracker.finalize(&artifact_store).unwrap();
        let stored = artifact_store.get(&artifact_id).unwrap().unwrap();
        assert_eq!(stored.kind, ArtifactKind::CodeDiff);
        let parsed: TickDiffSummary = serde_json::from_slice(&stored.content).unwrap();
        assert!(parsed.is_summary);
        assert_eq!(parsed, summary);
    }

    #[test]
    fn diff_tracker_event_summary_fields() {
        let mut tracker = DiffTracker::new(10);
        tracker.record(
            ArtifactId::from_content(b"diff-a"),
            sample_content("src/a.rs", CodeDiffOperation::Edit, 4, 2),
        );
        tracker.record(
            ArtifactId::from_content(b"diff-b"),
            sample_content("src/b.rs", CodeDiffOperation::Patch, 1, 3),
        );

        let summary = tracker.build_event_summary().unwrap();
        assert_eq!(summary.files_modified, 2);
        assert_eq!(summary.lines_added, 5);
        assert_eq!(summary.lines_removed, 5);
        assert_eq!(summary.net_delta, 0);
        assert_eq!(summary.files.len(), 2);
    }

    #[test]
    fn diff_tracker_format_for_reflect() {
        let mut tracker = DiffTracker::new(11);
        tracker.record(
            ArtifactId::from_content(b"diff-a"),
            sample_content("src/main.rs", CodeDiffOperation::Edit, 3, 1),
        );
        tracker.record(
            ArtifactId::from_content(b"diff-b"),
            sample_content("tests/main.rs", CodeDiffOperation::Create, 7, 0),
        );

        let rendered = tracker.format_for_reflect();
        assert!(rendered.contains("## File Changes This Tick"));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("tests/main.rs"));
        assert!(rendered.contains("Modified 2 file(s) (+10 lines, -1 lines)"));
    }

    #[test]
    fn diff_tracker_format_for_reflect_empty() {
        let tracker = DiffTracker::new(12);
        assert!(tracker.format_for_reflect().is_empty());
    }
}
