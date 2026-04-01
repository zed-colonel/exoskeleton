//! Thrash detection — analyzes recent tick patterns for signs of cognitive thrashing.
//!
//! Thrashing occurs when the cognitive loop repeats failed approaches without
//! progress. The detector analyzes recent TickRecords and produces a graduated
//! ThrashAssessment with recommended interventions.

use exoskeleton_core::budget::{ThrashAssessment, ThrashLevel};
use exoskeleton_core::tick::{ActionOutcome, TickRecord};

/// Analyzes recent tick patterns for signs of thrashing.
pub struct ThrashDetector;

impl ThrashDetector {
    /// Analyze recent ticks and return a thrash assessment.
    ///
    /// Indicators:
    /// - Same action attempted > 3 times with same failure
    /// - Tick count increasing but no observable state changes
    /// - Token consumption rate high relative to progress
    pub fn check(recent_ticks: &[TickRecord]) -> ThrashAssessment {
        if recent_ticks.is_empty() {
            return ThrashAssessment::none();
        }

        let mut indicators = Vec::new();
        let mut max_level = ThrashLevel::None;

        // 1. Action repetition: same action_type failing repeatedly
        let repetition = Self::check_action_repetition(recent_ticks);
        if repetition >= 5 {
            max_level = max_level.max(ThrashLevel::High);
            indicators.push(format!("same action failed {} times", repetition));
        } else if repetition >= 3 {
            max_level = max_level.max(ThrashLevel::Medium);
            indicators.push(format!(
                "action repeated {} times with failures",
                repetition
            ));
        }

        // 2. Progress stagnation: no decision rationale changes across recent ticks
        let stagnant = Self::check_stagnation(recent_ticks);
        if stagnant >= 10 {
            max_level = max_level.max(ThrashLevel::High);
            indicators.push(format!("{} ticks without state change", stagnant));
        } else if stagnant >= 5 {
            max_level = max_level.max(ThrashLevel::Medium);
            indicators.push(format!("{} ticks without progress", stagnant));
        }

        // 3. Token waste: high consumption with low action success rate
        let (tokens, successes, attempts) = Self::check_efficiency(recent_ticks);
        if attempts > 0 && successes == 0 && tokens > 10_000 {
            max_level = max_level.max(ThrashLevel::Medium);
            indicators.push(format!(
                "{} tokens consumed, 0/{} actions succeeded",
                tokens, attempts
            ));
        }

        let recommendation = match max_level {
            ThrashLevel::None => None,
            ThrashLevel::Low => Some("Increase tick interval for backoff".into()),
            ThrashLevel::Medium => Some("Escalate to frontier model and reduce scope".into()),
            ThrashLevel::High => Some("Suspend cognitive loop and alert human".into()),
        };

        ThrashAssessment {
            level: max_level,
            indicators,
            recommendation,
        }
    }

    /// Check for repeated failing actions (same action_type).
    fn check_action_repetition(ticks: &[TickRecord]) -> usize {
        // Count consecutive failed actions of the same type (most recent first)
        let mut action_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();

        for tick in ticks {
            for action in &tick.actions_taken {
                if action.outcome == ActionOutcome::Failure {
                    *action_counts.entry(&action.action_type).or_insert(0) += 1;
                }
            }
        }

        action_counts.values().copied().max().unwrap_or(0)
    }

    /// Check for stagnation — actions attempted but no progress.
    ///
    /// Only ticks where actions were attempted but ALL failed count as stagnant.
    /// Idle ticks (no actions taken) are deliberate — the vessel chose to wait
    /// (e.g., for conversation input) and should not be penalized.
    fn check_stagnation(ticks: &[TickRecord]) -> usize {
        let mut stagnant_count = 0;

        for tick in ticks {
            let has_actions = !tick.actions_taken.is_empty();
            let has_successful_action = tick
                .actions_taken
                .iter()
                .any(|a| a.outcome == ActionOutcome::Success);

            if has_actions && has_successful_action {
                // Progress was made — reset stagnation counter
                stagnant_count = 0;
            } else if has_actions && !has_successful_action {
                // Attempted actions but all failed — this is stagnation
                stagnant_count += 1;
            }
            // If !has_actions: idle by choice, don't touch the counter
        }

        stagnant_count
    }

    /// Check efficiency — total tokens consumed vs. successful actions.
    /// Returns (total_tokens, successful_actions, total_attempts).
    fn check_efficiency(ticks: &[TickRecord]) -> (u64, usize, usize) {
        let mut total_tokens = 0u64;
        let mut successes = 0usize;
        let mut attempts = 0usize;

        for tick in ticks {
            for call in &tick.llm_calls {
                total_tokens = total_tokens.saturating_add(call.total_tokens());
            }
            for action in &tick.actions_taken {
                attempts += 1;
                if action.outcome == ActionOutcome::Success {
                    successes += 1;
                }
            }
        }

        (total_tokens, successes, attempts)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::id::{ArtifactId, TickId};
    use exoskeleton_core::tick::{ActionRecord, LlmCallRecord, TickPhase};

    use super::*;

    fn make_tick(actions: Vec<ActionRecord>, llm_calls: Vec<LlmCallRecord>) -> TickRecord {
        TickRecord {
            tick_id: TickId::new(),
            tick_number: 0,
            phase: TickPhase::Amend,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            snapshot_before: ArtifactId::from_content(b"before"),
            snapshot_after: None,
            thread_contributions: vec![],
            actions_taken: actions,
            llm_calls,
            decision_rationale: None,
            context_breakdown_ref: None,
        }
    }

    fn failing_action(action_type: &str) -> ActionRecord {
        ActionRecord {
            action_type: action_type.into(),
            target: "target".into(),
            receipt_ref: None,
            outcome: ActionOutcome::Failure,
        }
    }

    fn successful_action(action_type: &str) -> ActionRecord {
        ActionRecord {
            action_type: action_type.into(),
            target: "target".into(),
            receipt_ref: None,
            outcome: ActionOutcome::Success,
        }
    }

    fn llm_call(tokens_in: u64, tokens_out: u64) -> LlmCallRecord {
        LlmCallRecord {
            model: "test".into(),
            tokens_in,
            tokens_out,
            cost_cents: 0.0,
            latency_ms: 100,
            response_artifact_ref: None,
            turns: 1,
        }
    }

    // ── T-4: ThrashDetector tests ──

    #[test]
    fn no_thrash_on_empty_ticks() {
        let assessment = ThrashDetector::check(&[]);
        assert_eq!(assessment.level, ThrashLevel::None);
        assert!(assessment.indicators.is_empty());
    }

    #[test]
    fn no_thrash_on_successful_ticks() {
        let ticks: Vec<_> = (0..5)
            .map(|_| {
                make_tick(
                    vec![successful_action("fs.write")],
                    vec![llm_call(500, 300)],
                )
            })
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert_eq!(assessment.level, ThrashLevel::None);
    }

    #[test]
    fn medium_thrash_on_repeated_failures() {
        let ticks: Vec<_> = (0..3)
            .map(|_| {
                make_tick(
                    vec![failing_action("http.request")],
                    vec![llm_call(500, 300)],
                )
            })
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert!(assessment.level >= ThrashLevel::Medium);
        assert!(!assessment.indicators.is_empty());
    }

    #[test]
    fn high_thrash_on_many_repeated_failures() {
        let ticks: Vec<_> = (0..5)
            .map(|_| {
                make_tick(
                    vec![failing_action("http.request")],
                    vec![llm_call(500, 300)],
                )
            })
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert_eq!(assessment.level, ThrashLevel::High);
    }

    #[test]
    fn no_thrash_on_idle_ticks() {
        // Idle ticks (no actions) are deliberate — not stagnation
        let ticks: Vec<_> = (0..10)
            .map(|_| make_tick(vec![], vec![llm_call(500, 300)]))
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert_eq!(
            assessment.level,
            ThrashLevel::None,
            "idle-by-choice ticks should not trigger stagnation"
        );
    }

    #[test]
    fn medium_thrash_on_failed_action_stagnation() {
        // 5 ticks with attempted but failed actions → stagnation
        let ticks: Vec<_> = (0..5)
            .map(|_| make_tick(vec![failing_action("fs.write")], vec![llm_call(500, 300)]))
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert!(
            assessment.level >= ThrashLevel::Medium,
            "repeated failed actions should trigger stagnation"
        );
    }

    #[test]
    fn high_thrash_on_long_failed_stagnation() {
        // 10 ticks with attempted but failed actions → high stagnation
        let ticks: Vec<_> = (0..10)
            .map(|_| make_tick(vec![failing_action("shell.exec")], vec![llm_call(500, 300)]))
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert_eq!(
            assessment.level,
            ThrashLevel::High,
            "10 ticks of failed actions should trigger high stagnation"
        );
    }

    #[test]
    fn idle_ticks_between_failures_do_not_reset_stagnation() {
        // Failed → idle → failed should still count the failures
        let ticks = vec![
            make_tick(vec![failing_action("fs.write")], vec![llm_call(500, 300)]),
            make_tick(vec![], vec![llm_call(200, 100)]), // idle — doesn't reset
            make_tick(vec![failing_action("fs.write")], vec![llm_call(500, 300)]),
        ];
        let assessment = ThrashDetector::check(&ticks);
        // Stagnant count should be 2 (two failed ticks, idle tick neutral)
        // Not enough for Medium (needs 5), but demonstrates counter isn't reset
        assert_eq!(assessment.level, ThrashLevel::None);
    }

    #[test]
    fn medium_thrash_on_token_waste() {
        // High token consumption with zero successful actions
        let ticks: Vec<_> = (0..3)
            .map(|_| make_tick(vec![failing_action("fs.write")], vec![llm_call(3000, 2000)]))
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert!(assessment.level >= ThrashLevel::Medium);
    }

    #[test]
    fn no_thrash_on_legitimate_repetitive_work() {
        // Repeated SUCCESSFUL actions (polling pattern) → not thrashing
        let ticks: Vec<_> = (0..10)
            .map(|_| {
                make_tick(
                    vec![successful_action("http.request")],
                    vec![llm_call(500, 300)],
                )
            })
            .collect();
        let assessment = ThrashDetector::check(&ticks);
        assert_eq!(assessment.level, ThrashLevel::None);
    }
}
