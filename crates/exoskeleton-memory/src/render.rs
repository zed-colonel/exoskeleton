//! Section renderers for context compilation.
//!
//! Each renderer is a pure function that takes domain types and produces clean,
//! structured text suitable for LLM consumption. Renderers are independent —
//! each can be tested in isolation.
//!
//! Section headers use the `=== SECTION NAME ===` format for clear delineation.
//! Renderers return empty string for empty input — the compiler skips those sections.

use exoskeleton_core::{
    EpisodicSummary, EventEntry, LongTermNote, PrincipalSummary, RelationshipSnapshot,
    StateSnapshot, ThreadContribution, VesselId,
};

/// Render the system section: vessel identity, mission, operating mode.
///
/// This is the highest-priority section — it establishes the LLM's identity
/// and purpose. Always present in the compiled context.
pub fn render_system_section(vessel_id: VesselId, mission: &str) -> String {
    format!(
        "=== SYSTEM ===\n\
         Vessel: {vessel_id}\n\
         Mission: {mission}\n\
         \n\
         You are an Exoskeleton vessel operating under the PODAARA cognitive loop \
         (Perceive \u{2192} Orient \u{2192} Decide \u{2192} Align \u{2192} Act \u{2192} \
         Reflect \u{2192} Amend).\n\
         Your decisions are bounded by budget constraints and relationship awareness.\n\
         Respond with structured decisions when asked."
    )
}

/// Render the state snapshot section: current vessel status, plan, budget.
pub fn render_snapshot_section(snapshot: &StateSnapshot) -> String {
    let mut s = format!(
        "=== STATE (Tick {}) ===\n\
         Status: {:?}\n",
        snapshot.tick_number, snapshot.status
    );
    if let Some(ref plan) = snapshot.plan {
        s.push_str(&format!("Plan: {plan}\n"));
    }
    if !snapshot.working_context.is_empty() {
        s.push_str(&format!("Focus: {}\n", snapshot.working_context));
    }
    s.push_str(&format!(
        "Budget: {} local + {} frontier tokens, {} cost-cents, {}s remaining\n",
        snapshot.budget_status.local_tokens_remaining,
        snapshot.budget_status.frontier_tokens_remaining,
        snapshot.budget_status.frontier_cost_cents_remaining,
        snapshot.budget_status.time_secs_remaining
    ));
    if let Some(ref summary) = snapshot.last_action_summary {
        s.push_str(&format!("Last action: {summary}\n"));
    }
    if !snapshot.thread_summaries.is_empty() {
        s.push_str("Active threads:\n");
        for ts in &snapshot.thread_summaries {
            s.push_str(&format!(
                "  - {} ({:?}): {}\n",
                ts.name,
                ts.status,
                ts.last_output_summary.as_deref().unwrap_or("no output yet")
            ));
        }
    }
    s
}

/// Render the relationship snapshot section.
///
/// Shows the vessel's current relationship state with all known principals.
/// Returns empty string if no relationship data exists.
pub fn render_relationship_section(snapshot: &RelationshipSnapshot) -> String {
    if snapshot.principals.is_empty() {
        return String::new();
    }
    let mut s = "=== RELATIONSHIPS ===\n".to_string();
    for p in &snapshot.principals {
        s.push_str(&render_principal(p));
    }
    s
}

fn render_principal(p: &PrincipalSummary) -> String {
    let mut s = format!(
        "- {} ({}): trust={:.2}, commitments={}\n",
        p.display_name, p.role, p.trust_level, p.active_commitments
    );
    if let Some(ref notes) = p.notes {
        s.push_str(&format!("  Notes: {notes}\n"));
    }
    s
}

/// Render thread contributions from the most recent tick.
///
/// Shows each thread's summary and recommendations.
/// Returns empty string if no contributions exist.
pub fn render_thread_outputs(contributions: &[ThreadContribution]) -> String {
    if contributions.is_empty() {
        return String::new();
    }
    let mut s = "=== THREAD OUTPUTS ===\n".to_string();
    for tc in contributions {
        s.push_str(&format!("- [{}]: {}\n", tc.thread_id, tc.summary));
    }
    s
}

/// Render recent events from the EventLedger.
///
/// Shows events in reverse chronological order (newest first).
/// Returns empty string if no events exist.
pub fn render_recent_events(events: &[EventEntry]) -> String {
    if events.is_empty() {
        return String::new();
    }
    let mut s = "=== RECENT EVENTS ===\n".to_string();
    for event in events {
        s.push_str(&format!(
            "- [{}] {:?}: {}\n",
            event.timestamp.format("%H:%M:%S"),
            event.event_type,
            event.summary
        ));
    }
    s
}

/// Render episodic memory summaries.
///
/// Shows summaries in reverse chronological order (most recent span first).
/// Returns empty string if no summaries exist.
pub fn render_episodic_memory(summaries: &[EpisodicSummary]) -> String {
    if summaries.is_empty() {
        return String::new();
    }
    let mut s = "=== EPISODIC MEMORY ===\n".to_string();
    for summary in summaries {
        s.push_str(&format!(
            "- Ticks {}-{}: {}\n",
            summary.start_tick, summary.end_tick, summary.summary
        ));
        for event in &summary.key_events {
            s.push_str(&format!("    * {event}\n"));
        }
    }
    s
}

/// Render long-term memory notes.
///
/// Shows notes grouped by topic.
/// Returns empty string if no notes exist.
pub fn render_long_term_memory(notes: &[LongTermNote]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut s = "=== LONG-TERM MEMORY ===\n".to_string();
    for note in notes {
        s.push_str(&format!("- [{}]: {}\n", note.topic, note.content));
        if !note.tags.is_empty() {
            s.push_str(&format!("  Tags: {}\n", note.tags.join(", ")));
        }
    }
    s
}

/// Render the working context section.
///
/// Shows the current task-specific focus text.
/// Returns empty string if working context is empty.
pub fn render_working_context(context: &str) -> String {
    if context.is_empty() {
        return String::new();
    }
    format!("=== WORKING CONTEXT ===\n{context}\n")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{
        ArtifactId, EventType, LedgerEntryId, PrincipalId, ThreadId, ThreadStatus, ThreadSummary,
        VesselId,
    };

    use super::*;

    // ── T-4: Section Renderers ──

    #[test]
    fn render_system_contains_mission() {
        let id = VesselId::new();
        let output = render_system_section(id, "Analyze security threats");
        assert!(output.contains("Analyze security threats"));
        assert!(output.contains(&id.to_string()));
    }

    #[test]
    fn render_system_contains_podaara() {
        let output = render_system_section(VesselId::new(), "test");
        assert!(output.contains("PODAARA"));
    }

    #[test]
    fn render_snapshot_contains_tick() {
        let mut snap = StateSnapshot::initial(VesselId::new(), "test".into());
        snap.tick_number = 42;
        let output = render_snapshot_section(&snap);
        assert!(output.contains("42"));
    }

    #[test]
    fn render_snapshot_with_plan() {
        let mut snap = StateSnapshot::initial(VesselId::new(), "test".into());
        snap.plan = Some("Execute plan A".into());
        let output = render_snapshot_section(&snap);
        assert!(output.contains("Plan: Execute plan A"));
    }

    #[test]
    fn render_snapshot_without_plan() {
        let snap = StateSnapshot::initial(VesselId::new(), "test".into());
        let output = render_snapshot_section(&snap);
        assert!(!output.contains("Plan:"));
    }

    #[test]
    fn render_snapshot_with_threads() {
        let mut snap = StateSnapshot::initial(VesselId::new(), "test".into());
        snap.thread_summaries = vec![
            ThreadSummary {
                thread_id: ThreadId::new(),
                name: "Threat Monitor".into(),
                status: ThreadStatus::Active,
                last_output_summary: Some("All clear".into()),
                token_budget_remaining: 5000,
            },
            ThreadSummary {
                thread_id: ThreadId::new(),
                name: "Self-Critique".into(),
                status: ThreadStatus::Active,
                last_output_summary: None,
                token_budget_remaining: 3000,
            },
        ];
        let output = render_snapshot_section(&snap);
        assert!(output.contains("Threat Monitor"));
        assert!(output.contains("Self-Critique"));
    }

    #[test]
    fn render_relationship_with_principals() {
        let snap = RelationshipSnapshot {
            principals: vec![
                PrincipalSummary {
                    principal_id: PrincipalId::new(),
                    display_name: "Keith".into(),
                    role: "operator".into(),
                    trust_level: 0.95,
                    active_commitments: 2,
                    last_interaction: None,
                    notes: Some("Primary operator".into()),
                },
                PrincipalSummary {
                    principal_id: PrincipalId::new(),
                    display_name: "Auditor".into(),
                    role: "auditor".into(),
                    trust_level: 0.5,
                    active_commitments: 0,
                    last_interaction: None,
                    notes: None,
                },
            ],
            compiled_at: Utc::now(),
        };
        let output = render_relationship_section(&snap);
        assert!(output.contains("Keith"));
        assert!(output.contains("Auditor"));
        assert!(output.contains("RELATIONSHIPS"));
    }

    #[test]
    fn render_relationship_empty() {
        let snap = RelationshipSnapshot {
            principals: Vec::new(),
            compiled_at: Utc::now(),
        };
        let output = render_relationship_section(&snap);
        assert!(output.is_empty());
    }

    #[test]
    fn render_thread_outputs_with_contributions() {
        let contributions = vec![
            ThreadContribution {
                thread_id: ThreadId::new(),
                artifact_id: ArtifactId::from_content(b"a"),
                summary: "No threats detected".into(),
            },
            ThreadContribution {
                thread_id: ThreadId::new(),
                artifact_id: ArtifactId::from_content(b"b"),
                summary: "Alignment check passed".into(),
            },
        ];
        let output = render_thread_outputs(&contributions);
        assert!(output.contains("No threats detected"));
        assert!(output.contains("Alignment check passed"));
        assert!(output.contains("THREAD OUTPUTS"));
    }

    #[test]
    fn render_thread_outputs_empty() {
        let output = render_thread_outputs(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn render_recent_events_with_events() {
        let events = vec![
            EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::VesselStarted,
                payload_ref: None,
                summary: "Vessel boot completed".into(),
                timestamp: Utc::now(),
            },
            EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::TickCompleted,
                payload_ref: None,
                summary: "Tick 1 finished".into(),
                timestamp: Utc::now(),
            },
            EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::ActionExecuted,
                payload_ref: None,
                summary: "Wrote file".into(),
                timestamp: Utc::now(),
            },
        ];
        let output = render_recent_events(&events);
        assert!(output.contains("Vessel boot completed"));
        assert!(output.contains("Tick 1 finished"));
        assert!(output.contains("Wrote file"));
        assert!(output.contains("RECENT EVENTS"));
    }

    #[test]
    fn render_recent_events_empty() {
        let output = render_recent_events(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn render_episodic_with_summaries() {
        let summaries = vec![
            EpisodicSummary {
                id: ArtifactId::from_content(b"ep1"),
                start_tick: 0,
                end_tick: 5,
                summary: "Initial exploration phase".into(),
                key_events: vec!["Discovered API endpoint".into()],
                token_count: 20,
                created_at: Utc::now(),
            },
            EpisodicSummary {
                id: ArtifactId::from_content(b"ep2"),
                start_tick: 6,
                end_tick: 10,
                summary: "Analysis phase".into(),
                key_events: Vec::new(),
                token_count: 15,
                created_at: Utc::now(),
            },
        ];
        let output = render_episodic_memory(&summaries);
        assert!(output.contains("Ticks 0-5"));
        assert!(output.contains("Initial exploration phase"));
        assert!(output.contains("Ticks 6-10"));
        assert!(output.contains("Analysis phase"));
        assert!(output.contains("EPISODIC MEMORY"));
    }

    #[test]
    fn render_episodic_empty() {
        let output = render_episodic_memory(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn render_long_term_with_notes() {
        let notes = vec![
            LongTermNote {
                id: ArtifactId::from_content(b"lt1"),
                topic: "architecture".into(),
                content: "Uses dual ActionQueue engines".into(),
                tags: vec!["design".into()],
                token_count: 10,
                created_at: Utc::now(),
            },
            LongTermNote {
                id: ArtifactId::from_content(b"lt2"),
                topic: "preferences".into(),
                content: "User prefers concise output".into(),
                tags: Vec::new(),
                token_count: 8,
                created_at: Utc::now(),
            },
        ];
        let output = render_long_term_memory(&notes);
        assert!(output.contains("architecture"));
        assert!(output.contains("Uses dual ActionQueue engines"));
        assert!(output.contains("preferences"));
        assert!(output.contains("User prefers concise output"));
        assert!(output.contains("LONG-TERM MEMORY"));
    }

    #[test]
    fn render_long_term_empty() {
        let output = render_long_term_memory(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn render_working_context_nonempty() {
        let output = render_working_context("Evaluating options for next action");
        assert!(output.contains("WORKING CONTEXT"));
        assert!(output.contains("Evaluating options"));
    }

    #[test]
    fn render_working_context_empty() {
        let output = render_working_context("");
        assert!(output.is_empty());
    }
}
