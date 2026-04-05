//! Unified diff parser and colorized renderer.
//!
//! Two rendering modes:
//! - **Full:** Parses unified diff text into file headers, hunk headers, add/remove/context
//!   lines with green/red/cyan/dim styling.
//! - **Summary:** Renders a compact summary from `DiffSummary` data when full text is unavailable.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// A parsed unified diff.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDiff {
    /// File-level sections in the diff.
    pub files: Vec<DiffFile>,
}

/// A single file's diff within a unified diff.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffFile {
    /// The file path (extracted from --- and +++ headers).
    pub path: String,
    /// Hunks within this file.
    pub hunks: Vec<DiffHunk>,
}

/// A single hunk within a file diff.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffHunk {
    /// The @@ header line (e.g., "@@ -10,3 +10,7 @@").
    pub header: String,
    /// Lines within the hunk.
    pub lines: Vec<DiffLine>,
}

/// A single line within a diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    /// Added line (starts with +).
    Added(String),
    /// Removed line (starts with -).
    Removed(String),
    /// Context line (unchanged, starts with space).
    Context(String),
}

/// Parse unified diff text into structured `ParsedDiff`.
pub fn parse_unified_diff(text: &str) -> ParsedDiff {
    if text.is_empty() {
        return ParsedDiff { files: Vec::new() };
    }

    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_path = String::new();
    let mut current_hunks: Vec<DiffHunk> = Vec::new();
    let mut current_hunk_header = String::new();
    let mut current_hunk_lines: Vec<DiffLine> = Vec::new();
    let mut in_hunk = false;

    for line in text.lines() {
        if line.starts_with("--- ") {
            if in_hunk {
                current_hunks.push(DiffHunk {
                    header: std::mem::take(&mut current_hunk_header),
                    lines: std::mem::take(&mut current_hunk_lines),
                });
                in_hunk = false;
            }
            if !current_path.is_empty() || !current_hunks.is_empty() {
                files.push(DiffFile {
                    path: std::mem::take(&mut current_path),
                    hunks: std::mem::take(&mut current_hunks),
                });
            }
            continue;
        }

        if line.starts_with("+++ ") {
            let raw_path = line.trim_start_matches('+').trim_start_matches(' ');
            current_path = raw_path
                .strip_prefix("b/")
                .or_else(|| raw_path.strip_prefix("a/"))
                .unwrap_or(raw_path)
                .to_string();
            continue;
        }

        if line.starts_with("@@ ") {
            if in_hunk {
                current_hunks.push(DiffHunk {
                    header: std::mem::take(&mut current_hunk_header),
                    lines: std::mem::take(&mut current_hunk_lines),
                });
            }
            current_hunk_header = line.to_string();
            in_hunk = true;
            continue;
        }

        if in_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                current_hunk_lines.push(DiffLine::Added(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                current_hunk_lines.push(DiffLine::Removed(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix(' ') {
                current_hunk_lines.push(DiffLine::Context(rest.to_string()));
            } else {
                current_hunk_lines.push(DiffLine::Context(line.to_string()));
            }
        }
    }

    if in_hunk {
        current_hunks.push(DiffHunk {
            header: current_hunk_header,
            lines: current_hunk_lines,
        });
    }
    if !current_path.is_empty() || !current_hunks.is_empty() {
        files.push(DiffFile {
            path: current_path,
            hunks: current_hunks,
        });
    }

    ParsedDiff { files }
}

/// Render a parsed unified diff to colorized ratatui Lines.
pub fn render_diff_lines(diff: &ParsedDiff) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for file in &diff.files {
        lines.push(Line::from(Span::styled(
            format!(
                "\u{2500}\u{2500}\u{2500} {} \u{2500}\u{2500}\u{2500}",
                file.path
            ),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        )));

        for hunk in &file.hunks {
            lines.push(Line::from(Span::styled(
                hunk.header.clone(),
                Style::default().fg(Color::Cyan),
            )));

            for diff_line in &hunk.lines {
                match diff_line {
                    DiffLine::Added(text) => lines.push(Line::from(Span::styled(
                        format!("+{text}"),
                        Style::default().fg(Color::Green),
                    ))),
                    DiffLine::Removed(text) => lines.push(Line::from(Span::styled(
                        format!("-{text}"),
                        Style::default().fg(Color::Red),
                    ))),
                    DiffLine::Context(text) => lines.push(Line::from(Span::styled(
                        format!(" {text}"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ))),
                }
            }
        }

        lines.push(Line::from(""));
    }

    lines
}

/// Render a full unified diff from text to colorized ratatui Lines.
pub fn render_diff_text(diff_text: &str) -> Vec<Line<'static>> {
    let parsed = parse_unified_diff(diff_text);
    render_diff_lines(&parsed)
}

/// Summary data for diff rendering when full text is unavailable.
///
/// This is a TUI-local struct populated from `exoskeleton_core::DiffSummary`
/// and `FileDiffEntry` to avoid coupling the renderer to core types.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffSummaryData {
    pub files_modified: u32,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub net_delta: i64,
    pub files: Vec<DiffFileSummary>,
}

/// Per-file summary for summary-only diff rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffFileSummary {
    pub path: String,
    pub operation: String,
    pub lines_added: i64,
    pub lines_removed: i64,
}

/// Render a diff summary (without full unified diff text) to ratatui Lines.
pub fn render_diff_summary(summary: &DiffSummaryData) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(
            format!(
                "{} file{} changed",
                summary.files_modified,
                if summary.files_modified == 1 { "" } else { "s" }
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            format!("+{}", summary.lines_added),
            Style::default().fg(Color::Green),
        ),
        Span::styled(" / ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("-{}", summary.lines_removed),
            Style::default().fg(Color::Red),
        ),
        Span::styled(
            format!(" (net {:+})", summary.net_delta),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    for file in &summary.files {
        let op_style = match file.operation.as_str() {
            "create" => Style::default().fg(Color::Green),
            "edit" | "write" | "patch" => Style::default().fg(Color::Yellow),
            "delete" => Style::default().fg(Color::Red),
            _ => Style::default().fg(Color::DarkGray),
        };

        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(format!("{:<6}", file.operation), op_style),
            Span::styled(" ", Style::default()),
            Span::styled(file.path.clone(), Style::default().fg(Color::White)),
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("+{}", file.lines_added),
                Style::default().fg(Color::Green),
            ),
            Span::styled("/", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("-{}", file.lines_removed),
                Style::default().fg(Color::Red),
            ),
        ]));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: collect all span content from lines into a single string.
    fn all_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    // ── T18: parse_unified_diff_single_hunk ──

    #[test]
    fn parse_unified_diff_single_hunk() {
        let diff = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    eprintln!(\"added\");
 }";
        let parsed = parse_unified_diff(diff);
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, "src/main.rs");
        assert_eq!(parsed.files[0].hunks.len(), 1);

        let hunk = &parsed.files[0].hunks[0];
        assert!(hunk.header.starts_with("@@ "));
        assert_eq!(hunk.lines.len(), 5);
        assert!(matches!(&hunk.lines[0], DiffLine::Context(text) if text.contains("fn main")));
        assert!(matches!(&hunk.lines[1], DiffLine::Removed(text) if text.contains("old")));
        assert!(matches!(&hunk.lines[2], DiffLine::Added(text) if text.contains("new")));
        assert!(matches!(&hunk.lines[3], DiffLine::Added(text) if text.contains("added")));
        assert!(matches!(&hunk.lines[4], DiffLine::Context(text) if text.contains('}')));
    }

    // ── T19: parse_unified_diff_multiple_hunks ──

    #[test]
    fn parse_unified_diff_multiple_hunks() {
        let diff = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
-old_first
+new_first
 context
@@ -10,3 +10,4 @@
 more context
+added line
 end";
        let parsed = parse_unified_diff(diff);
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].hunks.len(), 2);
        assert_eq!(parsed.files[0].hunks[0].lines.len(), 3);
        assert_eq!(parsed.files[0].hunks[1].lines.len(), 3);
    }

    // ── T20: parse_unified_diff_empty ──

    #[test]
    fn parse_unified_diff_empty() {
        let parsed = parse_unified_diff("");
        assert!(parsed.files.is_empty());
    }

    // ── T21: render_diff_lines_coloring ──

    #[test]
    fn render_diff_lines_coloring() {
        let diff = parse_unified_diff(
            "\
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 context
-removed
+added",
        );
        let lines = render_diff_lines(&diff);

        let hunk_line = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.starts_with("@@")));
        assert!(hunk_line.is_some(), "should have @@ hunk header");
        let hunk_span = &hunk_line.unwrap().spans[0];
        assert_eq!(hunk_span.style.fg, Some(Color::Cyan), "@@ should be cyan");

        let add_line = lines.iter().find(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("+added"))
        });
        assert!(add_line.is_some(), "should have added line");
        let add_span = &add_line.unwrap().spans[0];
        assert_eq!(add_span.style.fg, Some(Color::Green), "+ should be green");

        let rem_line = lines.iter().find(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("-removed"))
        });
        assert!(rem_line.is_some(), "should have removed line");
        let rem_span = &rem_line.unwrap().spans[0];
        assert_eq!(rem_span.style.fg, Some(Color::Red), "- should be red");

        let ctx_line = lines.iter().find(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("context"))
        });
        assert!(ctx_line.is_some(), "should have context line");
        let ctx_span = &ctx_line.unwrap().spans[0];
        assert_eq!(
            ctx_span.style.fg,
            Some(Color::DarkGray),
            "context should be dim/gray"
        );
    }

    // ── T22: render_diff_file_header ──

    #[test]
    fn render_diff_file_header() {
        let diff = parse_unified_diff(
            "\
--- a/src/config.rs
+++ b/src/config.rs
@@ -1,1 +1,1 @@
-old
+new",
        );
        let lines = render_diff_lines(&diff);

        let header_line = lines.iter().find(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("src/config.rs"))
        });
        assert!(header_line.is_some(), "should have file header");
        let header_span = &header_line.unwrap().spans[0];
        assert!(
            header_span.style.add_modifier.contains(Modifier::BOLD),
            "file header should be bold"
        );
        assert!(
            header_span.content.contains('\u{2500}'),
            "file header should have horizontal rule characters"
        );
    }

    // ── T23: render_diff_summary_only ──

    #[test]
    fn render_diff_summary_only() {
        let summary = DiffSummaryData {
            files_modified: 2,
            lines_added: 15,
            lines_removed: 3,
            net_delta: 12,
            files: vec![
                DiffFileSummary {
                    path: "src/main.rs".into(),
                    operation: "edit".into(),
                    lines_added: 10,
                    lines_removed: 2,
                },
                DiffFileSummary {
                    path: "tests/test.rs".into(),
                    operation: "create".into(),
                    lines_added: 5,
                    lines_removed: 1,
                },
            ],
        };

        let lines = render_diff_summary(&summary);
        let text = all_text(&lines);

        assert!(
            text.contains("2 files changed"),
            "should show file count: got: {text}"
        );
        assert!(text.contains("+15"), "should show lines added");
        assert!(text.contains("-3"), "should show lines removed");
        assert!(text.contains("src/main.rs"), "should show file path");
        assert!(
            text.contains("tests/test.rs"),
            "should show second file path"
        );
        assert!(text.contains("edit"), "should show operation");
        assert!(text.contains("create"), "should show operation");
    }
}
