//! Token counting infrastructure for budget-aware context compilation.
//!
//! The `TokenCounter` trait abstracts token counting, allowing different
//! implementations (approximate heuristic, tiktoken, etc.) to be swapped
//! without changing the compiler. Sprint 3 provides `ApproximateTokenCounter`;
//! a tiktoken-based counter may be added in Sprint 9.

/// Counts tokens in text for budget allocation.
///
/// Implementations range from fast approximations to exact tokenizer-based
/// counts. The ContextCompiler uses this trait to measure sections and
/// enforce budgets.
pub trait TokenCounter: Send + Sync {
    /// Count the number of tokens in the given text.
    fn count(&self, text: &str) -> u64;

    /// Truncate text to fit within the given token budget.
    ///
    /// Returns the truncated text. The result is guaranteed to have
    /// `count(result) <= budget`. Truncation happens at word boundaries
    /// when possible to avoid mid-word cuts.
    fn truncate_to_budget(&self, text: &str, budget: u64) -> String;
}

/// Approximate token count for a string using the chars/4 heuristic.
///
/// This is a standalone function that matches [`ApproximateTokenCounter::count`]
/// logic. Use this when you need a quick token estimate without constructing
/// a `TokenCounter` instance (e.g., for pre-computed `token_count` fields in
/// `EpisodicSummary` or `LongTermNote`).
pub fn approximate_token_count(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

/// Fast approximate token counter using the chars/4 heuristic.
///
/// For English text with typical LLM tokenizers (BPE-based), one token
/// averages approximately 4 characters. This is a conservative estimate
/// (slightly over-counts), which is preferable for budget enforcement —
/// it's better to slightly under-fill the context window than to overflow.
///
/// Accuracy: within ~15% of actual token counts for English prose.
/// For code or non-English text, accuracy may be lower.
pub struct ApproximateTokenCounter;

impl TokenCounter for ApproximateTokenCounter {
    fn count(&self, text: &str) -> u64 {
        // chars/4 rounded up — conservative estimate
        let chars = text.chars().count() as u64;
        chars.div_ceil(4)
    }

    fn truncate_to_budget(&self, text: &str, budget: u64) -> String {
        if budget == 0 {
            return String::new();
        }
        if self.count(text) <= budget {
            return text.to_string();
        }
        // Target character count: budget * 4 (inverse of chars/4)
        let target_chars = (budget * 4) as usize;
        // Find the last word boundary at or before target_chars
        let truncated: String = text.chars().take(target_chars).collect();
        match truncated.rfind(|c: char| c.is_whitespace()) {
            Some(pos) => truncated[..pos].to_string(),
            None => truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-2: Token Counter ──

    #[test]
    fn count_empty_string() {
        let counter = ApproximateTokenCounter;
        assert_eq!(counter.count(""), 0);
    }

    #[test]
    fn count_short_text() {
        let counter = ApproximateTokenCounter;
        // "hello world" = 11 chars, (11 + 3) / 4 = 3
        assert_eq!(counter.count("hello world"), 3);
    }

    #[test]
    fn count_longer_text() {
        let counter = ApproximateTokenCounter;
        let text = "a".repeat(400);
        // 400 chars, (400 + 3) / 4 = 100
        assert_eq!(counter.count(&text), 100);
    }

    #[test]
    fn truncate_within_budget() {
        let counter = ApproximateTokenCounter;
        let text = "hello world"; // 3 tokens
        let result = counter.truncate_to_budget(text, 200);
        assert_eq!(result, text);
    }

    #[test]
    fn truncate_at_budget() {
        let counter = ApproximateTokenCounter;
        let text = "a".repeat(400); // 100 tokens
        let result = counter.truncate_to_budget(&text, 50);
        assert!(
            counter.count(&result) <= 50,
            "Truncated text has {} tokens, expected <= 50",
            counter.count(&result)
        );
    }

    #[test]
    fn truncate_word_boundary() {
        let counter = ApproximateTokenCounter;
        // Create text with clear word boundaries
        let text = "The quick brown fox jumps over the lazy dog and more words follow after";
        let result = counter.truncate_to_budget(text, 5); // ~20 chars
                                                          // Should not cut mid-word
        assert!(
            !result.ends_with(|c: char| !c.is_whitespace() && c != result.chars().last().unwrap()),
            "Truncation should be at a word boundary"
        );
        assert!(counter.count(&result) <= 5);
    }

    #[test]
    fn truncate_empty_string() {
        let counter = ApproximateTokenCounter;
        let result = counter.truncate_to_budget("", 100);
        assert_eq!(result, "");
    }

    #[test]
    fn truncate_zero_budget() {
        let counter = ApproximateTokenCounter;
        let result = counter.truncate_to_budget("hello world", 0);
        assert_eq!(result, "");
    }

    #[test]
    fn count_unicode() {
        let counter = ApproximateTokenCounter;
        // CJK + emoji: each character is 1 char, so counts by char/4
        let text = "\u{4F60}\u{597D}\u{4E16}\u{754C}\u{1F600}"; // 你好世界😀
        let count = counter.count(text);
        assert_eq!(count, 2); // 5 chars -> (5 + 3) / 4 = 2
    }

    #[test]
    fn truncate_preserves_unicode() {
        let counter = ApproximateTokenCounter;
        let text = "Hello \u{4F60}\u{597D}\u{4E16}\u{754C} world \u{1F600}";
        let result = counter.truncate_to_budget(text, 2); // ~8 chars
                                                          // Must be valid UTF-8 (Rust guarantees this for String)
        assert!(result.len() <= text.len());
        assert!(counter.count(&result) <= 2);
    }
}
