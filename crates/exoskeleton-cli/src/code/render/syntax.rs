//! Syntax highlighting via syntect — converts code strings to ratatui styled Lines.
//!
//! The `SyntaxHighlighter` lazily loads syntect's default syntax definitions and
//! a dark theme on first use. All common languages (Rust, Python, TypeScript, Go,
//! JSON, TOML, YAML, Markdown, Bash, etc.) are included automatically via
//! `SyntaxSet::load_defaults_newlines()`.

use std::sync::OnceLock;

use ratatui::{
    prelude::Stylize,
    style::{Color, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

/// Global syntax set, loaded once.
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();

/// Global theme set, loaded once.
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

/// The theme name used for syntax highlighting.
const THEME_NAME: &str = "base16-ocean.dark";

/// Get or initialize the global syntax set.
fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Get or initialize the global theme set.
fn theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Highlight a block of code with the given language tag.
///
/// Returns ratatui `Line`s with syntax-colored spans. If the language is not
/// recognized, returns plain text spans with a dim foreground.
pub fn highlight_code(code: &str, language: &str) -> Vec<Line<'static>> {
    let ss = syntax_set();
    let ts = theme_set();

    let syntax = ss
        .find_syntax_by_token(language)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let theme = ts.themes.get(THEME_NAME).unwrap_or_else(|| {
        ts.themes
            .values()
            .next()
            .expect("syntect must have at least one theme")
    });

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();

    for line_str in LinesWithEndings::from(code) {
        let highlighted = match highlighter.highlight_line(line_str, ss) {
            Ok(ranges) => ranges,
            Err(_) => {
                lines.push(Line::from(Span::styled(
                    line_str.trim_end_matches('\n').to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
                continue;
            }
        };

        let spans: Vec<Span<'static>> = highlighted
            .into_iter()
            .map(|(style, text)| {
                let fg = syntect_color_to_ratatui(style.foreground);
                let mut ratatui_style = Style::default().fg(fg);
                if style.font_style.contains(FontStyle::BOLD) {
                    ratatui_style = ratatui_style.bold();
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    ratatui_style = ratatui_style.italic();
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    ratatui_style = ratatui_style.underlined();
                }
                Span::styled(text.trim_end_matches('\n').to_string(), ratatui_style)
            })
            .collect();

        lines.push(Line::from(spans));
    }

    lines
}

/// Convert a syntect RGBA color to the nearest ratatui Color.
fn syntect_color_to_ratatui(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T15: highlight_rust_code ──

    #[test]
    fn highlight_rust_code() {
        let code = "fn main() {\n    println!(\"hello\");\n}\n";
        let lines = highlight_code(code, "rs");
        assert!(!lines.is_empty(), "should produce at least one line");
        let has_colored_span = lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| !matches!(span.style.fg, None | Some(Color::Reset)))
        });
        assert!(has_colored_span, "Rust code should have colored spans");
    }

    // ── T16: highlight_unknown_lang_falls_back ──

    #[test]
    fn highlight_unknown_lang_falls_back() {
        let code = "some random stuff\nline two\n";
        let lines = highlight_code(code, "completely_unknown_language_xyz");
        assert!(
            !lines.is_empty(),
            "should produce lines even for unknown language"
        );
        let all_text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            all_text.contains("some random stuff"),
            "content should be preserved"
        );
    }

    // ── T17: highlight_empty_code ──

    #[test]
    fn highlight_empty_code() {
        let lines = highlight_code("", "rs");
        assert!(lines.is_empty() || lines.len() <= 1);
    }
}
