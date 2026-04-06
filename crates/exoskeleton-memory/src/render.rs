//! Section renderers for context compilation.
//!
//! Each renderer is a pure function that takes domain types and produces clean,
//! structured text suitable for LLM consumption. Renderers are independent —
//! each can be tested in isolation.
//!
//! Section headers use the `=== SECTION NAME ===` format for clear delineation.
//! Renderers return empty string for empty input — the compiler skips those sections.

use std::collections::HashMap;

use exoskeleton_core::conversation::{Conversation, ConversationState};
use exoskeleton_core::plan::{Plan, PlanTaskStatus};
use exoskeleton_core::working_memory::WorkingMemory;
use exoskeleton_core::{
    ArtifactId, EpisodicSummary, EventEntry, LongTermNote, PrincipalSummary, RelationshipSnapshot,
    StateSnapshot, ThreadContribution, VesselId, WorkingMemoryEntry,
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

/// Render the plan section with status indicators.
pub fn render_plan(plan: &Plan) -> String {
    let mut s = format!("=== PLAN ===\nObjective: {}\n", plan.objective);
    if plan.tasks.is_empty() {
        s.push_str("(no tasks defined)\n");
        return s;
    }
    for task in &plan.tasks {
        let indicator = match task.status {
            PlanTaskStatus::Pending => "[ ]",
            PlanTaskStatus::InProgress => "[>]",
            PlanTaskStatus::Completed => "[x]",
            PlanTaskStatus::Failed => "[!]",
            PlanTaskStatus::Blocked => "[#]",
            PlanTaskStatus::Skipped => "[-]",
        };
        s.push_str(&format!(
            "{} {} ({})\n",
            indicator, task.description, task.id
        ));
        if !task.depends_on.is_empty() {
            let deps: Vec<String> = task.depends_on.iter().map(|d| d.to_string()).collect();
            s.push_str(&format!("    depends on: {}\n", deps.join(", ")));
        }
        if let Some(ref hint) = task.tool_hint {
            s.push_str(&format!("    tool: {hint}\n"));
        }
    }
    s
}

/// Render working memory sorted by relevance (desc), with TTL info.
pub fn render_working_memory(memory: &WorkingMemory) -> String {
    if memory.is_empty() {
        return String::new();
    }
    let mut s = "=== WORKING MEMORY ===\n".to_string();
    let mut sorted: Vec<&WorkingMemoryEntry> = memory.entries.iter().collect();
    sorted.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for entry in sorted {
        s.push_str(&format!("- {}: {}", entry.key, entry.value));
        if let Some(ttl) = entry.ttl_ticks {
            s.push_str(&format!(
                " (ttl: {}t, written: t{})",
                ttl, entry.written_at_tick
            ));
        }
        s.push_str(&format!(" [rel: {:.1}]\n", entry.relevance));
    }
    s
}

/// Render active conversations for the LLM context.
///
/// Shows each conversation's participants, message count, and the last few
/// messages with their resolved content. When `resolved_content` is provided,
/// the actual message text is displayed; otherwise falls back to envelope
/// references (useful for tests or when artifact resolution is unavailable).
///
/// Conversations are shown in the order provided (typically newest-updated
/// first from the ConversationStore).
/// Returns empty string if no conversations exist.
pub fn render_conversations(
    conversations: &[Conversation],
    resolved_content: Option<&HashMap<ArtifactId, String>>,
) -> String {
    if conversations.is_empty() {
        return String::new();
    }
    let mut s = "=== CONVERSATIONS ===\n".to_string();
    for conv in conversations {
        let participants: Vec<String> = conv.participants.iter().map(|p| p.to_string()).collect();
        let status = match conv.state {
            ConversationState::Active => "active",
            ConversationState::Stale => "stale",
            ConversationState::Closed => "closed",
        };
        s.push_str(&format!(
            "- {} [{}] ({} msgs, {})\n",
            conv.topic.as_deref().unwrap_or("(untitled)"),
            conv.id,
            conv.message_refs.len(),
            status,
        ));
        s.push_str(&format!("  Participants: {}\n", participants.join(", ")));
        // Show last 3 messages (most recent context)
        let recent: Vec<_> = conv.message_refs.iter().rev().take(3).collect();
        for msg in recent.into_iter().rev() {
            let content = resolved_content
                .and_then(|map| map.get(&msg.payload_ref))
                .map(|text| text.as_str());
            match content {
                Some(text) => {
                    s.push_str(&format!(
                        "  [{} {}]: {}\n",
                        msg.source,
                        msg.timestamp.format("%H:%M:%S"),
                        text,
                    ));
                }
                None => {
                    s.push_str(&format!(
                        "  [{} {}]: (unresolved envelope:{})\n",
                        msg.source,
                        msg.timestamp.format("%H:%M:%S"),
                        msg.envelope_id,
                    ));
                }
            }
        }
    }
    s
}

/// Render pre-assembled repo instructions.
pub fn render_repo_instructions(section: &str) -> String {
    section.to_string()
}

/// Render pre-assembled git context.
pub fn render_git_context(section: &str) -> String {
    section.to_string()
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
    fn render_snapshot_without_plan() {
        let snap = StateSnapshot::initial(VesselId::new(), "test".into());
        let output = render_snapshot_section(&snap);
        // Plan is now a separate section, not in snapshot
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

    // ── E1-T56: render_plan with tasks shows status indicators ──
    #[test]
    fn render_plan_with_tasks() {
        use std::collections::HashMap;

        use exoskeleton_core::plan::{PlanTask, PlanTaskStatus};
        use exoskeleton_core::PlanTaskId;

        let plan = Plan {
            objective: "Achieve goal X".into(),
            tasks: vec![
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "First task".into(),
                    status: PlanTaskStatus::Completed,
                    depends_on: vec![],
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Second task".into(),
                    status: PlanTaskStatus::Pending,
                    depends_on: vec![],
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
                PlanTask {
                    id: PlanTaskId::new(),
                    description: "Failed task".into(),
                    status: PlanTaskStatus::Failed,
                    depends_on: vec![],
                    tool_hint: None,
                    metadata: HashMap::new(),
                },
            ],
            updated_at: Utc::now(),
        };
        let output = render_plan(&plan);
        assert!(output.contains("[x]"));
        assert!(output.contains("[ ]"));
        assert!(output.contains("[!]"));
        assert!(output.contains("PLAN"));
        assert!(output.contains("Achieve goal X"));
    }

    // ── E1-T57: render_plan with dependencies ──
    #[test]
    fn render_plan_with_dependencies() {
        use std::collections::HashMap;

        use exoskeleton_core::plan::{PlanTask, PlanTaskStatus};
        use exoskeleton_core::PlanTaskId;

        let dep_id = PlanTaskId::new();
        let plan = Plan {
            objective: "Test".into(),
            tasks: vec![PlanTask {
                id: PlanTaskId::new(),
                description: "Depends on another".into(),
                status: PlanTaskStatus::Blocked,
                depends_on: vec![dep_id],
                tool_hint: Some("fs.write".into()),
                metadata: HashMap::new(),
            }],
            updated_at: Utc::now(),
        };
        let output = render_plan(&plan);
        assert!(output.contains("depends on:"));
        assert!(output.contains("tool: fs.write"));
        assert!(output.contains("[#]"));
    }

    // ── E1-T58: render_plan empty tasks ──
    #[test]
    fn render_plan_empty_tasks() {
        let plan = Plan {
            objective: "Empty plan".into(),
            tasks: vec![],
            updated_at: Utc::now(),
        };
        let output = render_plan(&plan);
        assert!(output.contains("(no tasks defined)"));
    }

    // ── E1-T59: render_working_memory sorts by relevance ──
    #[test]
    fn render_working_memory_sorted_by_relevance() {
        use exoskeleton_core::working_memory::WorkingMemoryEntry;

        let memory = WorkingMemory {
            entries: vec![
                WorkingMemoryEntry {
                    key: "low".into(),
                    value: "low-rel".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 0.1,
                },
                WorkingMemoryEntry {
                    key: "high".into(),
                    value: "high-rel".into(),
                    written_at_tick: 0,
                    ttl_ticks: None,
                    relevance: 0.9,
                },
            ],
        };
        let output = render_working_memory(&memory);
        let high_pos = output.find("high").unwrap();
        let low_pos = output.find("low").unwrap();
        assert!(high_pos < low_pos, "high relevance should appear first");
    }

    // ── E1-T60: render_working_memory shows TTL info ──
    #[test]
    fn render_working_memory_shows_ttl() {
        use exoskeleton_core::working_memory::WorkingMemoryEntry;

        let memory = WorkingMemory {
            entries: vec![WorkingMemoryEntry {
                key: "obs".into(),
                value: "something".into(),
                written_at_tick: 5,
                ttl_ticks: Some(10),
                relevance: 0.8,
            }],
        };
        let output = render_working_memory(&memory);
        assert!(output.contains("ttl: 10t"));
        assert!(output.contains("written: t5"));
        assert!(output.contains("[rel: 0.8]"));
    }

    // ── E1-T61: render_working_memory empty ──
    #[test]
    fn render_working_memory_empty() {
        let memory = WorkingMemory::new();
        let output = render_working_memory(&memory);
        assert!(output.is_empty());
    }

    // ── E1-T55: render_conversations with conversations ──
    #[test]
    fn render_conversations_with_conversations() {
        use exoskeleton_core::conversation::Conversation;
        use exoskeleton_core::EnvelopeId;

        let p1 = PrincipalId::new();
        let now = Utc::now();
        let mut conv = Conversation::from_first_message(
            p1,
            EnvelopeId::new(),
            ArtifactId::from_content(b"msg1"),
            now,
        );
        conv.topic = Some("Debugging issue #42".into());

        let output = super::render_conversations(&[conv], None);
        assert!(output.contains("CONVERSATIONS"));
        assert!(output.contains("Debugging issue #42"));
        assert!(output.contains("1 msgs"));
        assert!(output.contains("active"));
        assert!(output.contains("Participants:"));
    }

    // ── E1-T56: render_conversations empty returns empty ──
    #[test]
    fn render_conversations_empty() {
        let output = super::render_conversations(&[], None);
        assert!(output.is_empty());
    }

    // ── E1-T57: render_conversations shows last 3 messages ──
    #[test]
    fn render_conversations_last_3_messages() {
        use exoskeleton_core::conversation::Conversation;
        use exoskeleton_core::EnvelopeId;

        let p = PrincipalId::new();
        let t0 = Utc::now();
        let mut conv = Conversation::from_first_message(
            p,
            EnvelopeId::new(),
            ArtifactId::from_content(b"m1"),
            t0,
        );
        for i in 1..5 {
            conv.add_message(
                p,
                EnvelopeId::new(),
                ArtifactId::from_content(format!("m{}", i + 1).as_bytes()),
                t0 + chrono::Duration::seconds(i),
            );
        }
        assert_eq!(conv.message_count(), 5);

        let output = super::render_conversations(&[conv], None);
        // Should show "5 msgs" but only last 3 message lines (unresolved fallback)
        assert!(output.contains("5 msgs"));
        let msg_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains("unresolved envelope:"))
            .collect();
        assert_eq!(msg_lines.len(), 3, "should show last 3 messages only");
    }

    // ── render_conversations resolves message content from map ──
    #[test]
    fn render_conversations_with_resolved_content() {
        use std::collections::HashMap;

        use exoskeleton_core::conversation::Conversation;
        use exoskeleton_core::EnvelopeId;

        let p1 = PrincipalId::new();
        let now = Utc::now();
        let payload1 = ArtifactId::from_content(b"msg1");
        let payload2 = ArtifactId::from_content(b"msg2");
        let mut conv =
            Conversation::from_first_message(p1, EnvelopeId::new(), payload1.clone(), now);
        conv.add_message(
            p1,
            EnvelopeId::new(),
            payload2.clone(),
            now + chrono::Duration::seconds(1),
        );
        conv.topic = Some("Test conversation".into());

        let mut content_map = HashMap::new();
        content_map.insert(payload1, "Hello, how can I help?".to_string());
        content_map.insert(payload2, "Please analyze the logs.".to_string());

        let output = super::render_conversations(&[conv], Some(&content_map));
        assert!(output.contains("Hello, how can I help?"));
        assert!(output.contains("Please analyze the logs."));
        // Should NOT contain envelope references when content is resolved
        assert!(!output.contains("unresolved envelope:"));
    }
}
