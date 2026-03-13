//! Cognitive thread domain primitives.
//!
//! Threads are Cognitive AQ child tasks (IBP §3.1). They produce artifacts
//! containing recommendations — they never invoke tools (IBP §3.4) and
//! never mutate the snapshot directly (IBP §4.3).

use serde::{Deserialize, Serialize};

use crate::id::{ArtifactId, ThreadId, TickId};

/// Declaration of a cognitive thread.
///
/// Threads are Cognitive AQ child tasks (IBP §3.1). They produce artifacts
/// containing recommendations — they never invoke tools (IBP §3.4) and
/// never mutate the snapshot directly (IBP §4.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSpec {
    /// Unique identity of this thread.
    pub thread_id: ThreadId,
    /// Human-readable thread name (e.g., "Threat Monitor").
    pub name: String,
    /// Purpose and responsibilities of this thread.
    pub charter: String,
    /// Execution priority relative to other threads.
    pub priority: ThreadPriority,
    /// Per-tick token budget allocation (Cognitive AQ budget, I6).
    pub token_budget: u64,
    /// When this thread should execute.
    pub schedule: ThreadSchedule,
}

/// Execution priority for cognitive threads.
///
/// Higher-priority threads get context budget preference and execute
/// first when resources are constrained. Declared in ascending order
/// so derived `Ord` gives `Background < Low < Normal < High < Critical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadPriority {
    /// Lowest priority — runs only when ample resources are available.
    Background,
    /// Below-normal priority.
    Low,
    /// Default priority.
    Normal,
    /// Above-normal priority — runs before Normal threads.
    High,
    /// Highest priority — runs first, always.
    Critical,
}

/// When a cognitive thread should execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadSchedule {
    /// Execute every tick.
    EveryTick,
    /// Execute every N ticks.
    EveryNTicks(u32),
    /// Execute only when explicitly requested.
    OnDemand,
}

/// Operational status of a cognitive thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    /// Thread is active and will execute on its schedule.
    Active,
    /// Thread is temporarily suspended (will not execute until resumed).
    Suspended,
    /// Thread has completed its task and will not execute again.
    Completed,
    /// Thread failed and will not execute again without intervention.
    Failed,
}

impl ThreadStatus {
    /// Whether the thread is in a terminal state (will not execute again).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Artifact produced by a cognitive thread during a tick (on Cognitive AQ).
///
/// Thread outputs are always artifacts — they never directly mutate the
/// Snapshot (IBP §4.3). They contain recommendations — never direct tool
/// invocations (IBP §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadOutput {
    /// Which thread produced this output.
    pub thread_id: ThreadId,
    /// Which tick this output was produced during.
    pub tick_id: TickId,
    /// Content-addressed reference to the output artifact.
    pub artifact_id: ArtifactId,
    /// One-line summary of the output.
    pub summary: String,
    /// Specific recommendations for the master loop's Decide step.
    pub recommendations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T-8: Thread Primitives ──

    #[test]
    fn thread_spec_roundtrip() {
        let spec = ThreadSpec {
            thread_id: ThreadId::new(),
            name: "Threat Monitor".into(),
            charter: "Monitor for alignment threats and adversarial patterns".into(),
            priority: ThreadPriority::High,
            token_budget: 5000,
            schedule: ThreadSchedule::EveryTick,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: ThreadSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, parsed);
    }

    #[test]
    fn thread_priority_ordering() {
        assert!(ThreadPriority::Background < ThreadPriority::Low);
        assert!(ThreadPriority::Low < ThreadPriority::Normal);
        assert!(ThreadPriority::Normal < ThreadPriority::High);
        assert!(ThreadPriority::High < ThreadPriority::Critical);
    }

    #[test]
    fn thread_priority_all_variants_roundtrip() {
        let variants = [
            ThreadPriority::Background,
            ThreadPriority::Low,
            ThreadPriority::Normal,
            ThreadPriority::High,
            ThreadPriority::Critical,
        ];
        for p in &variants {
            let json = serde_json::to_string(p).unwrap();
            let parsed: ThreadPriority = serde_json::from_str(&json).unwrap();
            assert_eq!(*p, parsed);
        }
    }

    #[test]
    fn thread_schedule_all_variants_roundtrip() {
        let variants = [
            ThreadSchedule::EveryTick,
            ThreadSchedule::EveryNTicks(5),
            ThreadSchedule::OnDemand,
        ];
        for s in &variants {
            let json = serde_json::to_string(s).unwrap();
            let parsed: ThreadSchedule = serde_json::from_str(&json).unwrap();
            assert_eq!(*s, parsed);
        }
    }

    #[test]
    fn thread_status_is_terminal() {
        assert!(ThreadStatus::Completed.is_terminal());
        assert!(ThreadStatus::Failed.is_terminal());
        assert!(!ThreadStatus::Active.is_terminal());
        assert!(!ThreadStatus::Suspended.is_terminal());
    }

    #[test]
    fn thread_output_roundtrip() {
        let output = ThreadOutput {
            thread_id: ThreadId::new(),
            tick_id: TickId::new(),
            artifact_id: ArtifactId::from_content(b"thread output data"),
            summary: "Detected potential alignment drift".into(),
            recommendations: vec![
                "Increase monitoring frequency".into(),
                "Request human confirmation on next action".into(),
            ],
        };
        let json = serde_json::to_string(&output).unwrap();
        let parsed: ThreadOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(output, parsed);
    }
}
