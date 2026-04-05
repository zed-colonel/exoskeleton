//! Markdown rendering pipeline: pulldown-cmark events to ratatui styled Lines.
//!
//! Converts raw markdown text (from LLM responses) into a `Vec<Line<'static>>`
//! suitable for display in ratatui Paragraph widgets. Handles:
//!
//! - **Bold**, *italic*, `inline code`
//! - # Headings (all levels)
//! - [Links](url)
//! - > Blockquotes
//! - Bullet lists (- item) and ordered lists (1. item)
//! - Horizontal rules (---)
//! - Fenced code blocks with syntax highlighting (delegated to render::syntax)
//!
//! For streaming compatibility (S5), pulldown-cmark's event-based parser handles
//! partial/incomplete markdown naturally — re-parse the full buffer on each delta.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::code::render::syntax::highlight_code;

/// Render a markdown string to ratatui Lines.
///
/// The `width` parameter is used for horizontal rules (fills to terminal width).
pub fn render_markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }

    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_SMART_PUNCTUATION;
    let parser = Parser::new_ext(text, options);

    let mut renderer = MarkdownRenderer::new(width);
    renderer.process(parser);
    renderer.finish()
}

/// Internal state machine that accumulates pulldown-cmark events into Lines.
struct MarkdownRenderer {
    /// Completed lines ready for output.
    lines: Vec<Line<'static>>,
    /// Spans accumulating for the current line.
    current_spans: Vec<Span<'static>>,
    /// Style stack for nested inline formatting.
    style_stack: Vec<Style>,
    /// Terminal width for horizontal rules.
    width: u16,
    /// Whether we are inside a fenced code block.
    in_code_block: bool,
    /// Language tag for current code block.
    code_block_lang: String,
    /// Accumulated text inside a code block.
    code_block_buffer: String,
    /// Blockquote nesting depth.
    blockquote_depth: u32,
    /// Whether we are inside a list item.
    in_list_item: bool,
    /// Current list prefix (e.g., "  * " or "  1. ").
    list_prefix: String,
    /// Ordered list counter stack (one per nesting level).
    ordered_list_counters: Vec<u64>,
    /// Whether the current list item has had its prefix emitted.
    list_prefix_emitted: bool,
}

impl MarkdownRenderer {
    fn new(width: u16) -> Self {
        Self {
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: vec![Style::default()],
            width,
            in_code_block: false,
            code_block_lang: String::new(),
            code_block_buffer: String::new(),
            blockquote_depth: 0,
            in_list_item: false,
            list_prefix: String::new(),
            ordered_list_counters: Vec::new(),
            list_prefix_emitted: false,
        }
    }

    /// Current effective style (top of stack).
    fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }

    /// Push a new style layer (inheriting from current).
    fn push_style(&mut self, modifier: Style) {
        let base = self.current_style();
        let merged = merge_styles(base, modifier);
        self.style_stack.push(merged);
    }

    /// Pop the top style layer.
    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    /// Flush current spans into a completed line.
    fn flush_line(&mut self) {
        if self.current_spans.is_empty() {
            self.lines.push(Line::from(""));
            return;
        }

        let mut spans = std::mem::take(&mut self.current_spans);

        if self.blockquote_depth > 0 {
            let prefix = "\u{2502} ".repeat(self.blockquote_depth as usize);
            spans.insert(
                0,
                Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            );
        }

        self.lines.push(Line::from(spans));
    }

    /// Add a text span with the current style.
    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let style = self.current_style();
        self.current_spans
            .push(Span::styled(text.to_string(), style));
    }

    /// Process all events from the parser.
    fn process<'a>(&mut self, parser: impl Iterator<Item = Event<'a>>) {
        for event in parser {
            self.handle_event(event);
        }
    }

    /// Handle a single pulldown-cmark event.
    fn handle_event(&mut self, event: Event<'_>) {
        if self.in_code_block {
            match event {
                Event::Text(text) | Event::Code(text) => {
                    self.code_block_buffer.push_str(&text);
                }
                Event::SoftBreak | Event::HardBreak => self.code_block_buffer.push('\n'),
                Event::End(TagEnd::CodeBlock) => self.end_code_block(),
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag_end) => self.end_tag(tag_end),
            Event::Text(text) => {
                if self.in_list_item && !self.list_prefix_emitted {
                    let prefix = self.list_prefix.clone();
                    self.current_spans
                        .push(Span::styled(prefix, Style::default().fg(Color::DarkGray)));
                    self.list_prefix_emitted = true;
                }
                self.push_text(&text);
            }
            Event::Code(code) => {
                self.current_spans.push(Span::styled(
                    format!("`{code}`"),
                    Style::default().fg(Color::Yellow).bg(Color::DarkGray),
                ));
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                let rule_width = (self.width as usize).saturating_sub(2).max(3);
                let rule = "\u{2500}".repeat(rule_width);
                self.current_spans.push(Span::styled(
                    rule,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ));
                self.flush_line();
            }
            _ => {}
        }
    }

    /// Handle a start tag.
    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.push_style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                );
                let marker = "#".repeat(level as usize);
                self.push_text(&format!("{marker} "));
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { dest_url, .. } => {
                self.push_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED),
                );
                self.code_block_lang = dest_url.to_string();
            }
            Tag::BlockQuote(_) => {
                self.blockquote_depth += 1;
                self.push_style(Style::default().fg(Color::Gray));
            }
            Tag::List(start_number) => {
                self.ordered_list_counters.push(start_number.unwrap_or(0));
            }
            Tag::Item => {
                self.in_list_item = true;
                self.list_prefix_emitted = false;
                if let Some(&counter) = self.ordered_list_counters.last() {
                    let indent = "  ".repeat(self.ordered_list_counters.len().saturating_sub(1));
                    if counter == 0 {
                        self.list_prefix = format!("{indent}  \u{2022} ");
                    } else {
                        self.list_prefix = format!("{indent}  {counter}. ");
                    }
                }
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.in_code_block = true;
                self.code_block_lang = lang;
                self.code_block_buffer.clear();
            }
            _ => {}
        }
    }

    /// Handle an end tag.
    fn end_tag(&mut self, tag_end: TagEnd) {
        match tag_end {
            TagEnd::Paragraph => {
                self.flush_line();
                self.lines.push(Line::from(""));
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_line();
                self.lines.push(Line::from(""));
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if !self.code_block_lang.is_empty() {
                    let url = std::mem::take(&mut self.code_block_lang);
                    self.current_spans.push(Span::styled(
                        format!(" ({url})"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.pop_style();
            }
            TagEnd::List(_) => {
                self.ordered_list_counters.pop();
            }
            TagEnd::Item => {
                self.in_list_item = false;
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                if let Some(counter) = self.ordered_list_counters.last_mut() {
                    if *counter > 0 {
                        *counter += 1;
                    }
                }
            }
            TagEnd::CodeBlock => self.end_code_block(),
            _ => {}
        }
    }

    /// End a fenced code block: highlight and emit lines.
    fn end_code_block(&mut self) {
        let lang = std::mem::take(&mut self.code_block_lang);
        let code = std::mem::take(&mut self.code_block_buffer);
        self.in_code_block = false;

        let code_bg = Style::default().bg(Color::Indexed(236));

        if !lang.is_empty() {
            self.lines.push(Line::from(Span::styled(
                format!("  {lang}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }

        let highlighted = if lang.is_empty() {
            code.lines()
                .map(|line| {
                    Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(Color::White).bg(Color::Indexed(236)),
                    ))
                })
                .collect::<Vec<_>>()
        } else {
            let hl_lines = highlight_code(&code, &lang);
            hl_lines
                .into_iter()
                .map(|line| {
                    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
                    spans.push(Span::styled("  ".to_string(), code_bg));
                    for mut span in line.spans {
                        span.style = span.style.bg(Color::Indexed(236));
                        spans.push(span);
                    }
                    Line::from(spans)
                })
                .collect::<Vec<_>>()
        };

        self.lines.extend(highlighted);
        self.lines.push(Line::from(""));
    }

    /// Consume the renderer and return the accumulated lines.
    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current_spans.is_empty() {
            self.flush_line();
        }
        self.lines
    }
}

/// Merge two styles, overlaying `overlay` on top of `base`.
///
/// Foreground, background, and modifiers from overlay take precedence if set.
fn merge_styles(base: Style, overlay: Style) -> Style {
    let mut merged = base;
    if let Some(fg) = overlay.fg {
        merged = merged.fg(fg);
    }
    if let Some(bg) = overlay.bg {
        merged = merged.bg(bg);
    }
    merged.add_modifier(overlay.add_modifier)
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

    /// Helper: find the first span matching a predicate.
    fn find_span<'a>(
        lines: &'a [Line<'static>],
        pred: impl Fn(&Span<'static>) -> bool,
    ) -> Option<&'a Span<'static>> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| pred(span))
    }

    // ── T1: render_bold_text ──

    #[test]
    fn render_bold_text() {
        let lines = render_markdown("**bold**", 80);
        let bold_span = find_span(&lines, |span| span.content.contains("bold"));
        assert!(bold_span.is_some(), "should have a span containing 'bold'");
        let span = bold_span.unwrap();
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "bold text should have BOLD modifier"
        );
    }

    // ── T2: render_italic_text ──

    #[test]
    fn render_italic_text() {
        let lines = render_markdown("*italic*", 80);
        let italic_span = find_span(&lines, |span| span.content.contains("italic"));
        assert!(
            italic_span.is_some(),
            "should have a span containing 'italic'"
        );
        let span = italic_span.unwrap();
        assert!(
            span.style.add_modifier.contains(Modifier::ITALIC),
            "italic text should have ITALIC modifier"
        );
    }

    // ── T3: render_inline_code ──

    #[test]
    fn render_inline_code() {
        let lines = render_markdown("use `code` here", 80);
        let code_span = find_span(&lines, |span| span.content.contains("code"));
        assert!(code_span.is_some(), "should have a span containing 'code'");
        let span = code_span.unwrap();
        assert_eq!(
            span.style.fg,
            Some(Color::Yellow),
            "inline code should be yellow"
        );
        assert_eq!(
            span.style.bg,
            Some(Color::DarkGray),
            "inline code should have dark gray background"
        );
    }

    // ── T4: render_heading ──

    #[test]
    fn render_heading() {
        let lines = render_markdown("# Heading", 80);
        let heading_span = find_span(&lines, |span| span.content.contains("Heading"));
        assert!(
            heading_span.is_some(),
            "should have a span containing 'Heading'"
        );
        let span = heading_span.unwrap();
        assert_eq!(span.style.fg, Some(Color::Blue), "heading should be blue");
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "heading should be bold"
        );
    }

    // ── T5: render_link ──

    #[test]
    fn render_link() {
        let lines = render_markdown("[click here](https://example.com)", 80);
        let link_span = find_span(&lines, |span| span.content.contains("click here"));
        assert!(
            link_span.is_some(),
            "should have a span containing 'click here'"
        );
        let span = link_span.unwrap();
        assert_eq!(span.style.fg, Some(Color::Cyan), "link should be cyan");
        assert!(
            span.style.add_modifier.contains(Modifier::UNDERLINED),
            "link should be underlined"
        );
        let url_span = find_span(&lines, |span| span.content.contains("example.com"));
        assert!(url_span.is_some(), "URL should be shown");
    }

    // ── T6: render_blockquote ──

    #[test]
    fn render_blockquote() {
        let lines = render_markdown("> quoted text", 80);
        let text = all_text(&lines);
        assert!(
            text.contains("quoted text"),
            "should contain the quote text"
        );
        let has_border = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains('\u{2502}'))
        });
        assert!(
            has_border,
            "blockquote should have vertical bar border prefix"
        );
    }

    // ── T7: render_bullet_list ──

    #[test]
    fn render_bullet_list() {
        let lines = render_markdown("- item one\n- item two", 80);
        let text = all_text(&lines);
        assert!(text.contains('\u{2022}'), "should contain bullet character");
        assert!(text.contains("item one"), "should contain first item");
        assert!(text.contains("item two"), "should contain second item");
    }

    // ── T8: render_ordered_list ──

    #[test]
    fn render_ordered_list() {
        let lines = render_markdown("1. first\n2. second", 80);
        let text = all_text(&lines);
        assert!(text.contains("1."), "should contain ordered number 1");
        assert!(text.contains("first"), "should contain first item");
        assert!(text.contains("second"), "should contain second item");
    }

    // ── T9: render_horizontal_rule ──

    #[test]
    fn render_horizontal_rule() {
        let lines = render_markdown("---", 80);
        let has_rule = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains('\u{2500}'))
        });
        assert!(
            has_rule,
            "horizontal rule should render as box-drawing dashes"
        );
    }

    // ── T10: render_fenced_code_block_without_lang ──

    #[test]
    fn render_fenced_code_block_without_lang() {
        let md = "```\nlet x = 1;\n```";
        let lines = render_markdown(md, 80);
        let text = all_text(&lines);
        assert!(
            text.contains("let x = 1"),
            "code block should contain the code"
        );
        let has_bg = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(Color::Indexed(236)))
        });
        assert!(has_bg, "code block should have dim background");
    }

    // ── T11: render_fenced_code_block_with_lang ──

    #[test]
    fn render_fenced_code_block_with_lang() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md, 80);
        let text = all_text(&lines);
        assert!(
            text.contains("fn") && text.contains("main"),
            "code block should contain the code"
        );
        let has_colored = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(_, _, _))))
        });
        assert!(
            has_colored,
            "syntax-highlighted code should have RGB-colored spans"
        );
    }

    // ── T12: render_mixed_markdown ──

    #[test]
    fn render_mixed_markdown() {
        let md = "# Title\n\nSome **bold** and *italic* text with `code`.\n\n- item\n\n---\n";
        let lines = render_markdown(md, 80);
        let text = all_text(&lines);
        assert!(text.contains("Title"), "should contain heading");
        assert!(text.contains("bold"), "should contain bold text");
        assert!(text.contains("italic"), "should contain italic text");
        assert!(text.contains("code"), "should contain inline code");
        assert!(text.contains('\u{2022}'), "should contain bullet");
        assert!(text.contains('\u{2500}'), "should contain horizontal rule");
    }

    // ── T13: render_empty_string ──

    #[test]
    fn render_empty_string() {
        let lines = render_markdown("", 80);
        assert!(lines.is_empty(), "empty input should produce no lines");
    }

    // ── T14: render_plain_text_no_formatting ──

    #[test]
    fn render_plain_text_no_formatting() {
        let lines = render_markdown("just plain text", 80);
        let text = all_text(&lines);
        assert!(
            text.contains("just plain text"),
            "plain text should render as-is"
        );
    }
}
