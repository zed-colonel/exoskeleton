//! Session compaction manager (E10-S2, W-99).
//!
//! Merges old episodic memory summaries to free up context budget.

use std::collections::HashSet;

use exoskeleton_core::{EpisodicSummary, ExoError};
use exoskeleton_memory::MemoryStore;

/// Configuration for session compaction.
pub struct CompactionConfig {
    /// Number of recent episodic summaries to preserve unchanged.
    pub preserve_recent: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { preserve_recent: 3 }
    }
}

/// Result of a compaction run.
#[derive(Debug)]
pub enum CompactionResult {
    /// No compaction needed.
    NoAction,
    /// Compaction performed.
    Compacted {
        /// Number of original entries merged.
        entries_merged: usize,
        /// Number of merged summaries written.
        summaries_written: usize,
    },
}

/// Check whether compaction should run.
pub fn should_compact(
    truncated_sections: &[String],
    episodic_count: u64,
    episodic_capacity: Option<u64>,
) -> bool {
    if !truncated_sections.is_empty() {
        return true;
    }

    if let Some(capacity) = episodic_capacity {
        if capacity > 0 {
            return episodic_count as f64 / capacity as f64 > 0.8;
        }
    }

    false
}

/// Run compaction on episodic memory.
pub fn run_compaction(
    memory_store: &dyn MemoryStore,
    config: &CompactionConfig,
) -> Result<CompactionResult, ExoError> {
    let all_summaries = memory_store.recent_episodic(1000)?;
    if all_summaries.len() <= config.preserve_recent {
        return Ok(CompactionResult::NoAction);
    }

    let to_compact = &all_summaries[config.preserve_recent..];
    if to_compact.is_empty() {
        return Ok(CompactionResult::NoAction);
    }

    let mut oldest_first = to_compact.to_vec();
    oldest_first.sort_by_key(|summary| summary.start_tick);
    let groups = group_consecutive(&oldest_first);

    let mut entries_merged = 0usize;
    let mut summaries_written = 0usize;

    for group in groups {
        if group.len() <= 1 {
            continue;
        }

        let merged = merge_summaries(&group);
        memory_store.write_episodic(&merged)?;
        summaries_written += 1;

        for summary in &group {
            memory_store.delete_episodic(&summary.id)?;
        }
        entries_merged += group.len();
    }

    if entries_merged == 0 {
        Ok(CompactionResult::NoAction)
    } else {
        Ok(CompactionResult::Compacted {
            entries_merged,
            summaries_written,
        })
    }
}

/// Group consecutive episodic summaries.
fn group_consecutive(summaries: &[EpisodicSummary]) -> Vec<Vec<EpisodicSummary>> {
    if summaries.is_empty() {
        return Vec::new();
    }

    let mut groups = Vec::new();
    let mut current_group = vec![summaries[0].clone()];
    for summary in &summaries[1..] {
        let prev_end = current_group
            .last()
            .expect("group should not be empty")
            .end_tick;
        if summary.start_tick <= prev_end + 2 {
            current_group.push(summary.clone());
        } else {
            groups.push(current_group);
            current_group = vec![summary.clone()];
        }
    }
    groups.push(current_group);
    groups
}

/// Merge a group of episodic summaries into a single summary.
fn merge_summaries(group: &[EpisodicSummary]) -> EpisodicSummary {
    assert!(!group.is_empty());

    let start_tick = group
        .iter()
        .map(|summary| summary.start_tick)
        .min()
        .unwrap();
    let end_tick = group.iter().map(|summary| summary.end_tick).max().unwrap();
    let summary = group
        .iter()
        .map(|entry| {
            let first_line = entry.summary.lines().next().unwrap_or(&entry.summary);
            if first_line.len() > 100 {
                format!("{}...", &first_line[..97])
            } else {
                first_line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("; ");

    // Deduplicate key events while preserving chronological order.
    let mut seen = HashSet::new();
    let key_events: Vec<String> = group
        .iter()
        .flat_map(|entry| entry.key_events.iter().cloned())
        .filter(|event| seen.insert(event.clone()))
        .take(10)
        .collect();

    EpisodicSummary::new(
        start_tick,
        end_tick,
        format!("Compacted ticks {start_tick}-{end_tick}: {summary}"),
        key_events,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteMemoryStore;

    fn make_summary(start: u64, end: u64, text: &str) -> EpisodicSummary {
        EpisodicSummary::new(
            start,
            end,
            text.to_string(),
            vec![format!("event in ticks {start}-{end}")],
        )
    }

    #[test]
    fn should_compact_when_sections_truncated() {
        let truncated = vec!["episodic_memory".to_string()];
        assert!(should_compact(&truncated, 10, Some(100)));
    }

    #[test]
    fn should_compact_at_80_percent_capacity() {
        let truncated: Vec<String> = vec![];
        assert!(should_compact(&truncated, 85, Some(100)));
    }

    #[test]
    fn should_not_compact_when_healthy() {
        let truncated: Vec<String> = vec![];
        assert!(!should_compact(&truncated, 50, Some(100)));
    }

    #[test]
    fn should_not_compact_without_capacity() {
        let truncated: Vec<String> = vec![];
        assert!(!should_compact(&truncated, 100, None));
    }

    #[test]
    fn merge_adjacent_summaries() {
        let summaries = vec![
            make_summary(1, 3, "Explored codebase"),
            make_summary(4, 6, "Edited parser module"),
            make_summary(7, 9, "Ran tests and fixed failures"),
        ];
        let merged = merge_summaries(&summaries);
        assert_eq!(merged.start_tick, 1);
        assert_eq!(merged.end_tick, 9);
        assert!(merged.summary.contains("Explored codebase"));
        assert!(merged.summary.contains("Edited parser module"));
        assert!(merged.summary.contains("Ran tests"));
        assert_eq!(merged.key_events.len(), 3);
    }

    #[test]
    fn group_consecutive_summaries() {
        let summaries = vec![
            make_summary(1, 3, "first"),
            make_summary(4, 6, "second"),
            make_summary(10, 12, "third"),
            make_summary(13, 15, "fourth"),
        ];
        let groups = group_consecutive(&summaries);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[1].len(), 2);
    }

    #[test]
    fn group_single_items_not_grouped() {
        let summaries = vec![
            make_summary(1, 3, "first"),
            make_summary(10, 12, "second"),
            make_summary(20, 22, "third"),
        ];
        let groups = group_consecutive(&summaries);
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn compact_preserves_recent() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        for i in 0..6 {
            let summary = make_summary(i * 3 + 1, i * 3 + 3, &format!("session {i}"));
            store.write_episodic(&summary).unwrap();
        }
        assert_eq!(store.count_episodic().unwrap(), 6);

        let result = run_compaction(&store, &CompactionConfig { preserve_recent: 3 }).unwrap();
        assert!(matches!(result, CompactionResult::Compacted { .. }));

        let remaining = store.recent_episodic(100).unwrap();
        assert_eq!(remaining.len(), 4);
    }

    #[test]
    fn compact_no_action_when_few_entries() {
        let store = SqliteMemoryStore::in_memory().unwrap();
        store
            .write_episodic(&make_summary(1, 3, "only one"))
            .unwrap();

        let result = run_compaction(&store, &CompactionConfig { preserve_recent: 3 }).unwrap();
        assert!(matches!(result, CompactionResult::NoAction));
    }
}
