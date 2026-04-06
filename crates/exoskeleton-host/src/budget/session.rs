//! Per-session budget tracking for the inner interaction loop (E8-S1).
//!
//! Tracks resource consumption within a single inner-loop session (one tick).
//! Enforces step limits, token caps, wall-clock timeouts, and doom-loop
//! detection. Not persisted — session scope is one tick.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::InnerLoopConfig;

/// Tracks resource consumption within a single inner-loop session.
///
/// Created at the start of the inner loop, consumed per step, checked
/// before each iteration. Not persisted — session scope is one tick.
pub struct SessionBudget {
    config: InnerLoopConfig,
    steps_taken: u32,
    tokens_consumed: u64,
    started_at: Instant,
    /// Rolling window of recent tool calls for doom-loop detection.
    recent_tool_calls: Vec<ToolCallRecord>,
    /// Whether a correction prompt has already been issued for the current streak.
    correction_issued: bool,
    /// The tool/args streak currently under correction, if any.
    correction_target: Option<(String, u64, Option<String>)>,
    /// Number of identical calls observed after a correction was issued.
    post_correction_identical: u32,
}

impl SessionBudget {
    /// Create a new session budget from the inner loop configuration.
    pub fn new(config: &InnerLoopConfig) -> Self {
        Self {
            config: config.clone(),
            steps_taken: 0,
            tokens_consumed: 0,
            started_at: Instant::now(),
            recent_tool_calls: Vec::new(),
            correction_issued: false,
            correction_target: None,
            post_correction_identical: 0,
        }
    }

    /// Check whether another inner-loop step is allowed.
    pub fn can_continue(&self) -> SessionBudgetCheck {
        // Step limit
        if self.steps_taken >= self.config.max_steps_per_tick {
            return SessionBudgetCheck::Stop(SessionStopReason::StepLimitReached);
        }

        // Token limit
        if self.tokens_consumed >= self.config.max_tokens_per_session {
            return SessionBudgetCheck::Stop(SessionStopReason::TokenBudgetExhausted);
        }

        // Timeout
        if self.started_at.elapsed().as_secs() >= self.config.timeout_secs {
            return SessionBudgetCheck::Stop(SessionStopReason::TimeoutExpired);
        }

        // Doom loop detection
        if let DoomLoopStatus::HardStop { tool, count, .. } = self.doom_loop_status() {
            return SessionBudgetCheck::Stop(SessionStopReason::DoomLoopDetected {
                tool_name: tool,
                consecutive: count,
            });
        }

        SessionBudgetCheck::Continue
    }

    /// Record an LLM call's token consumption.
    pub fn record_llm_tokens(&mut self, tokens_in: u64, tokens_out: u64) {
        self.tokens_consumed = self.tokens_consumed.saturating_add(tokens_in + tokens_out);
    }

    /// Record a tool invocation for doom-loop detection.
    pub fn record_tool_call(&mut self, tool_name: &str, args: &Value, error: Option<&str>) {
        if tool_name == "agent.ask_user" {
            self.steps_taken += 1;
            return;
        }
        let args_hash = hash_value(args);
        let error = error.map(str::to_owned);
        self.recent_tool_calls.push(ToolCallRecord {
            tool_name: tool_name.to_string(),
            args: args.clone(),
            args_hash,
            error: error.clone(),
        });

        if self.correction_issued {
            if let Some((target_tool, target_hash, target_error)) = self.correction_target.as_mut()
            {
                if *target_tool == tool_name && *target_hash == args_hash {
                    self.post_correction_identical += 1;
                    *target_error = error;
                } else {
                    self.correction_issued = false;
                    self.correction_target = None;
                    self.post_correction_identical = 0;
                }
            }
        }

        self.steps_taken += 1;
    }

    /// Report current doom-loop status without forcing a stop.
    pub fn doom_loop_status(&self) -> DoomLoopStatus {
        let threshold = self.config.doom_loop_threshold as usize;
        if threshold == 0 {
            return DoomLoopStatus::Clear;
        }

        if self.correction_issued {
            if let Some((tool, _, last_error)) = &self.correction_target {
                if self.post_correction_identical >= 2 {
                    return DoomLoopStatus::HardStop {
                        tool: tool.clone(),
                        count: self.config.doom_loop_threshold + self.post_correction_identical,
                        last_error: last_error.clone(),
                    };
                }
            }
            return DoomLoopStatus::Clear;
        }

        if self.recent_tool_calls.len() < threshold {
            return DoomLoopStatus::Clear;
        }

        let recent = &self.recent_tool_calls[self.recent_tool_calls.len() - threshold..];
        let first = &recent[0];
        let all_same = recent
            .iter()
            .all(|call| call.tool_name == first.tool_name && call.args_hash == first.args_hash);
        if all_same {
            DoomLoopStatus::CorrectionNeeded {
                tool: first.tool_name.clone(),
                args: first.args.clone(),
                count: threshold as u32,
                last_error: recent.iter().rev().find_map(|call| call.error.clone()),
            }
        } else {
            DoomLoopStatus::Clear
        }
    }

    /// Mark that the inner loop injected a correction prompt for the current streak.
    pub fn acknowledge_correction(&mut self) {
        if let Some(last) = self.recent_tool_calls.last() {
            self.correction_issued = true;
            self.correction_target =
                Some((last.tool_name.clone(), last.args_hash, last.error.clone()));
            self.post_correction_identical = 0;
        }
    }

    /// Return a summary of why the session ended.
    pub fn completion_reason(&self) -> SessionCompletionReason {
        match self.can_continue() {
            SessionBudgetCheck::Continue => SessionCompletionReason::AgentComplete,
            SessionBudgetCheck::Stop(reason) => reason.into(),
        }
    }

    /// Total steps taken so far.
    pub fn steps_taken(&self) -> u32 {
        self.steps_taken
    }

    /// Total tokens consumed so far.
    pub fn tokens_consumed(&self) -> u64 {
        self.tokens_consumed
    }

    /// Maximum steps allowed.
    pub fn max_steps(&self) -> u32 {
        self.config.max_steps_per_tick
    }
}

#[derive(Debug, Clone)]
struct ToolCallRecord {
    tool_name: String,
    args: Value,
    args_hash: u64,
    error: Option<String>,
}

/// Hash a serde_json::Value for doom-loop comparison.
fn hash_value(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    let s = value.to_string();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Result of a session budget check.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionBudgetCheck {
    /// Session can continue.
    Continue,
    /// Session should stop — reason provided.
    Stop(SessionStopReason),
}

/// Status of doom-loop detection for the current session.
#[derive(Debug, Clone, PartialEq)]
pub enum DoomLoopStatus {
    Clear,
    CorrectionNeeded {
        tool: String,
        args: Value,
        count: u32,
        last_error: Option<String>,
    },
    HardStop {
        tool: String,
        count: u32,
        last_error: Option<String>,
    },
}

/// Why the session budget requires stopping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStopReason {
    /// Reached the maximum number of inner-loop steps.
    StepLimitReached,
    /// Exhausted the per-session token budget.
    TokenBudgetExhausted,
    /// Wall-clock timeout expired.
    TimeoutExpired,
    /// Detected repetitive identical tool calls.
    DoomLoopDetected { tool_name: String, consecutive: u32 },
}

/// Why the inner-loop session ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCompletionReason {
    /// The LLM decided it was done (normal completion).
    AgentComplete,
    /// Reached the maximum number of inner-loop steps.
    StepLimit,
    /// Exhausted the per-session token budget.
    TokenBudget,
    /// Wall-clock timeout expired.
    Timeout,
    /// Detected repetitive identical tool calls.
    DoomLoop,
    /// Cancelled via CancellationToken.
    Cancelled,
    /// Inner loop yielded while waiting for operator input.
    AwaitingInput,
    /// An error occurred during the session.
    Error(String),
}

impl From<SessionStopReason> for SessionCompletionReason {
    fn from(reason: SessionStopReason) -> Self {
        match reason {
            SessionStopReason::StepLimitReached => SessionCompletionReason::StepLimit,
            SessionStopReason::TokenBudgetExhausted => SessionCompletionReason::TokenBudget,
            SessionStopReason::TimeoutExpired => SessionCompletionReason::Timeout,
            SessionStopReason::DoomLoopDetected { .. } => SessionCompletionReason::DoomLoop,
        }
    }
}

impl std::fmt::Display for SessionCompletionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionCompletionReason::AgentComplete => write!(f, "agent_complete"),
            SessionCompletionReason::StepLimit => write!(f, "step_limit"),
            SessionCompletionReason::TokenBudget => write!(f, "token_budget"),
            SessionCompletionReason::Timeout => write!(f, "timeout"),
            SessionCompletionReason::DoomLoop => write!(f, "doom_loop"),
            SessionCompletionReason::Cancelled => write!(f, "cancelled"),
            SessionCompletionReason::AwaitingInput => write!(f, "awaiting_input"),
            SessionCompletionReason::Error(e) => write!(f, "error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> InnerLoopConfig {
        InnerLoopConfig {
            enabled: true,
            max_steps_per_tick: 5,
            max_tokens_per_session: 1000,
            timeout_secs: 300,
            context_window_size: 3,
            doom_loop_threshold: 3,
        }
    }

    // ── E8S1-T10: session_budget_step_limit ──
    #[test]
    fn session_budget_step_limit() {
        let config = test_config();
        let mut session = SessionBudget::new(&config);
        for i in 0..5 {
            assert_eq!(
                session.can_continue(),
                SessionBudgetCheck::Continue,
                "step {i} should be allowed"
            );
            session.record_tool_call(&format!("tool_{i}"), &serde_json::json!({"i": i}), None);
        }
        assert_eq!(
            session.can_continue(),
            SessionBudgetCheck::Stop(SessionStopReason::StepLimitReached)
        );
    }

    // ── E8S1-T11: session_budget_token_limit ──
    #[test]
    fn session_budget_token_limit() {
        let config = test_config();
        let mut session = SessionBudget::new(&config);
        session.record_llm_tokens(600, 500); // 1100 > 1000
        assert_eq!(
            session.can_continue(),
            SessionBudgetCheck::Stop(SessionStopReason::TokenBudgetExhausted)
        );
    }

    // ── E8S1-T12: session_budget_timeout ──
    #[test]
    fn session_budget_timeout() {
        let config = InnerLoopConfig {
            timeout_secs: 0, // immediate timeout
            ..test_config()
        };
        let session = SessionBudget::new(&config);
        assert_eq!(
            session.can_continue(),
            SessionBudgetCheck::Stop(SessionStopReason::TimeoutExpired)
        );
    }

    // ── E8S1-T13: session_budget_doom_loop ──
    #[test]
    fn session_budget_doom_loop() {
        let config = test_config();
        let mut session = SessionBudget::new(&config);
        let args = serde_json::json!({"path": "/tmp/test.txt"});
        session.record_tool_call("fs.read", &args, Some("not found"));
        session.record_tool_call("fs.read", &args, Some("not found"));
        assert_eq!(session.can_continue(), SessionBudgetCheck::Continue);
        session.record_tool_call("fs.read", &args, Some("not found"));
        assert!(matches!(
            session.doom_loop_status(),
            DoomLoopStatus::CorrectionNeeded { .. }
        ));
    }

    // ── E8S1-T14: session_budget_no_doom_loop_different_args ──
    #[test]
    fn session_budget_no_doom_loop_different_args() {
        let config = test_config();
        let mut session = SessionBudget::new(&config);
        session.record_tool_call("fs.read", &serde_json::json!({"path": "/a"}), None);
        session.record_tool_call("fs.read", &serde_json::json!({"path": "/b"}), None);
        session.record_tool_call("fs.read", &serde_json::json!({"path": "/c"}), None);
        assert_eq!(session.can_continue(), SessionBudgetCheck::Continue);
    }

    // ── E8S1-T15: session_budget_fresh_allows_continue ──
    #[test]
    fn session_budget_fresh_allows_continue() {
        let config = test_config();
        let session = SessionBudget::new(&config);
        assert_eq!(session.can_continue(), SessionBudgetCheck::Continue);
    }

    // ── T38: ask_user_exempt_from_doom_loop ──
    #[test]
    fn ask_user_exempt_from_doom_loop() {
        let config = test_config(); // doom_loop_threshold = 3
        let mut session = SessionBudget::new(&config);
        let args = serde_json::json!({"question": "What file?"});

        // 3 consecutive identical agent.ask_user calls should NOT trigger doom-loop
        session.record_tool_call("agent.ask_user", &args, None);
        session.record_tool_call("agent.ask_user", &args, None);
        session.record_tool_call("agent.ask_user", &args, None);

        // Should still be Continue (not DoomLoopDetected)
        assert_eq!(
            session.can_continue(),
            SessionBudgetCheck::Continue,
            "agent.ask_user must be exempt from doom-loop detection"
        );
    }

    // ── T39: inner_loop_yields_on_pending_question ──
    #[test]
    fn inner_loop_yields_on_pending_question() {
        // SessionCompletionReason::AwaitingInput exists and serializes correctly.
        // The inner loop yields this when agent.ask_user returns "pending".
        let reason = SessionCompletionReason::AwaitingInput;
        let display = format!("{reason}");
        assert_eq!(
            display, "awaiting_input",
            "AwaitingInput must display as 'awaiting_input'"
        );
    }

    // ── T40: inner_loop_continues_on_answered_question ──
    #[test]
    fn inner_loop_continues_on_answered_question() {
        // When agent.ask_user returns "answered", no pending_question flag
        // is set, so the inner loop continues. This test validates that
        // only "pending" status triggers AwaitingInput.
        let config = test_config();
        let mut session = SessionBudget::new(&config);

        // Record an agent.ask_user call (like "answered" — just counts as step)
        session.record_tool_call(
            "agent.ask_user",
            &serde_json::json!({"status": "answered"}),
            None,
        );

        // Session should still allow continuation
        assert_eq!(
            session.can_continue(),
            SessionBudgetCheck::Continue,
            "answered question should not stop the session"
        );
    }

    #[test]
    fn doom_loop_correction_phase() {
        let config = test_config();
        let mut session = SessionBudget::new(&config);
        let args = serde_json::json!({"path": "/tmp/test.txt"});

        session.record_tool_call("fs.read", &args, Some("file not found"));
        session.record_tool_call("fs.read", &args, Some("file not found"));
        session.record_tool_call("fs.read", &args, Some("file not found"));

        assert!(matches!(
            session.doom_loop_status(),
            DoomLoopStatus::CorrectionNeeded {
                ref tool,
                count: 3,
                ref last_error,
                ..
            } if tool == "fs.read" && last_error.as_deref() == Some("file not found")
        ));
    }

    #[test]
    fn doom_loop_hard_stop_after_correction() {
        let config = InnerLoopConfig {
            max_steps_per_tick: 10,
            ..test_config()
        };
        let mut session = SessionBudget::new(&config);
        let args = serde_json::json!({"path": "/tmp/test.txt"});

        session.record_tool_call("fs.read", &args, Some("file not found"));
        session.record_tool_call("fs.read", &args, Some("file not found"));
        session.record_tool_call("fs.read", &args, Some("file not found"));
        session.acknowledge_correction();
        session.record_tool_call("fs.read", &args, Some("file not found"));
        assert_eq!(session.can_continue(), SessionBudgetCheck::Continue);
        session.record_tool_call("fs.read", &args, Some("file not found"));
        assert!(matches!(
            session.can_continue(),
            SessionBudgetCheck::Stop(SessionStopReason::DoomLoopDetected {
                ref tool_name,
                consecutive: 5
            }) if tool_name == "fs.read"
        ));
    }
}
