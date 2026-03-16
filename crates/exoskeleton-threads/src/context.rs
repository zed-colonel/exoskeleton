//! Thread context compiler -- compiles token-budgeted context slices for threads.
//!
//! Each thread gets a focused context built from its charter, the vessel's state,
//! and its own recent outputs. Context is compiled fresh each tick (IBP 4.2, I5).

use exoskeleton_core::{ExoError, StateSnapshot, ThreadOutput, ThreadSpec, TickId};
use exoskeleton_memory::{CompiledContext, SectionResult, TokenCounter};

/// Compile a context slice for a single cognitive thread.
///
/// Assembles a token-budgeted prompt from the thread's charter, the current
/// vessel state snapshot, and the thread's recent outputs. The charter section
/// is never truncated; snapshot and output sections are truncated proportionally
/// if the budget is tight.
#[allow(clippy::too_many_arguments)]
pub fn compile_thread_context(
    counter: &dyn TokenCounter,
    thread: &ThreadSpec,
    snapshot: &StateSnapshot,
    recent_outputs: &[ThreadOutput],
    _tick_id: TickId,
    charter_template: Option<&str>,
) -> Result<CompiledContext, ExoError> {
    // ── 1. Render sections as text ──

    let charter_section = match charter_template {
        Some(template) => template.to_string(),
        None => format!(
            "=== THREAD: {} ===\n\
             Thread ID: {}\n\
             Charter: {}\n\
             Priority: {:?}\n\
             Current Tick: {}\n\
             \n\
             You are a cognitive thread within an Exoskeleton vessel.\n\
             Your purpose is described by your charter above.\n\
             Analyze the context below and produce:\n\
             1. A concise summary of your analysis\n\
             2. Specific recommendations for the master loop\n\
             \n\
             Respond with JSON:\n\
             {{\n\
               \"summary\": \"Your analysis in one sentence\",\n\
               \"recommendations\": [\"Specific actionable recommendation\", ...]\n\
             }}",
            thread.name, thread.thread_id, thread.charter, thread.priority, snapshot.tick_number,
        ),
    };

    let plan_display = snapshot.plan.as_deref().unwrap_or("None");
    let last_action_display = snapshot.last_action_summary.as_deref().unwrap_or("None");

    let snapshot_section = format!(
        "=== VESSEL STATE ===\n\
         Mission: {}\n\
         Status: {:?}\n\
         Plan: {}\n\
         Working Context: {}\n\
         Last Action: {}",
        snapshot.mission,
        snapshot.status,
        plan_display,
        snapshot.working_context,
        last_action_display,
    );

    let outputs_section = if recent_outputs.is_empty() {
        String::new()
    } else {
        let mut lines = vec!["=== YOUR RECENT OUTPUTS ===".to_string()];
        for output in recent_outputs {
            let recs = output.recommendations.join(", ");
            lines.push(format!(
                "- Tick {}: {}\n  Recommendations: {}",
                output.tick_id, output.summary, recs,
            ));
        }
        lines.join("\n")
    };

    // ── 2. Count tokens for each section ──

    let charter_tokens = counter.count(&charter_section);
    let snapshot_tokens = counter.count(&snapshot_section);
    let outputs_tokens = counter.count(&outputs_section);

    // ── 3. Check charter fits budget ──

    if charter_tokens > thread.token_budget {
        return Err(ExoError::Config(
            "thread charter exceeds token budget".into(),
        ));
    }

    // ── 4. Budget allocation ──
    // Charter gets its full count. Remaining is split 50/50 between
    // snapshot and outputs.

    let remaining = thread.token_budget - charter_tokens;
    let snap_budget = remaining / 2;
    let out_budget = remaining - snap_budget;

    // ── 5. Truncate if needed ──

    let (final_snapshot, snap_used, snap_truncated) = if snapshot_tokens > snap_budget {
        let truncated_text = counter.truncate_to_budget(&snapshot_section, snap_budget);
        let used = counter.count(&truncated_text);
        (truncated_text, used, true)
    } else {
        (snapshot_section.clone(), snapshot_tokens, false)
    };

    let (final_outputs, out_used, out_truncated) = if outputs_tokens > out_budget {
        let truncated_text = counter.truncate_to_budget(&outputs_section, out_budget);
        let used = counter.count(&truncated_text);
        (truncated_text, used, true)
    } else {
        (outputs_section.clone(), outputs_tokens, false)
    };

    // ── 6. Assemble prompt ──

    let mut parts: Vec<&str> = vec![&charter_section];
    if !final_snapshot.is_empty() {
        parts.push(&final_snapshot);
    }
    if !final_outputs.is_empty() {
        parts.push(&final_outputs);
    }
    let prompt = parts.join("\n\n");

    // ── 7. Count total tokens ──

    let total_tokens = counter.count(&prompt);

    // ── 8. Build CompiledContext ──

    let mut truncated_sections = Vec::new();
    if snap_truncated {
        truncated_sections.push("snapshot".into());
    }
    if out_truncated {
        truncated_sections.push("recent_outputs".into());
    }

    Ok(CompiledContext {
        prompt,
        total_tokens,
        budget: thread.token_budget,
        sections: vec![
            SectionResult {
                name: "charter".into(),
                allocated: charter_tokens,
                used: charter_tokens,
                truncated: false,
            },
            SectionResult {
                name: "snapshot".into(),
                allocated: snap_budget,
                used: snap_used,
                truncated: snap_truncated,
            },
            SectionResult {
                name: "recent_outputs".into(),
                allocated: out_budget,
                used: out_used,
                truncated: out_truncated,
            },
        ],
        truncated_sections,
    })
}

#[cfg(test)]
mod tests {
    use exoskeleton_core::{
        ArtifactId, ThreadId, ThreadPriority, ThreadSchedule, ThreadSpec, TickId, VesselId,
    };
    use exoskeleton_memory::ApproximateTokenCounter;

    use super::*;

    fn test_thread(name: &str, budget: u64) -> ThreadSpec {
        ThreadSpec {
            thread_id: ThreadId::new(),
            name: name.into(),
            charter: "Monitor for alignment threats".into(),
            priority: ThreadPriority::High,
            token_budget: budget,
            schedule: ThreadSchedule::EveryTick,
        }
    }

    fn make_output(thread_id: ThreadId, summary: &str) -> ThreadOutput {
        ThreadOutput {
            thread_id,
            tick_id: TickId::new(),
            artifact_id: ArtifactId::from_content(summary.as_bytes()),
            summary: summary.into(),
            recommendations: vec!["Take corrective action".into()],
        }
    }

    #[test]
    fn context_includes_charter() {
        let counter = ApproximateTokenCounter;
        let thread = test_thread("ThreatMon", 5000);
        let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());

        let ctx =
            compile_thread_context(&counter, &thread, &snapshot, &[], TickId::new(), None).unwrap();

        assert!(
            ctx.prompt.contains("Monitor for alignment threats"),
            "Prompt should contain the charter text"
        );
        assert!(
            ctx.prompt.contains("ThreatMon"),
            "Prompt should contain the thread name"
        );
    }

    #[test]
    fn context_includes_snapshot() {
        let counter = ApproximateTokenCounter;
        let thread = test_thread("SnapCheck", 5000);
        let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());

        let ctx =
            compile_thread_context(&counter, &thread, &snapshot, &[], TickId::new(), None).unwrap();

        assert!(
            ctx.prompt.contains("test mission"),
            "Prompt should contain mission"
        );
        assert!(
            ctx.prompt.contains("VESSEL STATE"),
            "Prompt should contain vessel state header"
        );
    }

    #[test]
    fn context_includes_recent_outputs() {
        let counter = ApproximateTokenCounter;
        let thread = test_thread("OutputCheck", 5000);
        let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());

        let outputs = vec![
            make_output(thread.thread_id, "Detected drift pattern"),
            make_output(thread.thread_id, "All clear"),
        ];

        let ctx =
            compile_thread_context(&counter, &thread, &snapshot, &outputs, TickId::new(), None)
                .unwrap();

        assert!(
            ctx.prompt.contains("Detected drift pattern"),
            "Prompt should contain first output summary"
        );
        assert!(
            ctx.prompt.contains("All clear"),
            "Prompt should contain second output summary"
        );
        assert!(
            ctx.prompt.contains("YOUR RECENT OUTPUTS"),
            "Prompt should contain outputs header"
        );
    }

    #[test]
    fn context_respects_token_budget() {
        let counter = ApproximateTokenCounter;
        let thread = test_thread("BudgetCheck", 5000);
        let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());

        let outputs = vec![
            make_output(thread.thread_id, "Output one"),
            make_output(thread.thread_id, "Output two"),
        ];

        let ctx =
            compile_thread_context(&counter, &thread, &snapshot, &outputs, TickId::new(), None)
                .unwrap();

        assert!(
            ctx.total_tokens <= thread.token_budget,
            "total_tokens={} exceeds budget={}",
            ctx.total_tokens,
            thread.token_budget
        );
    }

    #[test]
    fn context_truncates_outputs_before_charter() {
        let counter = ApproximateTokenCounter;
        // Very tight budget -- just enough for charter.
        let charter_text = "Monitor for alignment threats";
        let charter_tokens = counter.count(&format!(
            "=== THREAD: Tight ===\n\
             Thread ID: 00000000-0000-0000-0000-000000000000\n\
             Charter: {charter_text}\n\
             Priority: High\n\
             Current Tick: 0\n\
             \n\
             You are a cognitive thread within an Exoskeleton vessel.\n\
             Your purpose is described by your charter above.\n\
             Analyze the context below and produce:\n\
             1. A concise summary of your analysis\n\
             2. Specific recommendations for the master loop\n\
             \n\
             Respond with JSON:\n\
             {{\n\
               \"summary\": \"Your analysis in one sentence\",\n\
               \"recommendations\": \
              [\"Specific actionable recommendation\", ...]\n\
             }}"
        ));
        // Budget = charter_tokens + small margin for snapshot, not enough
        // for outputs.
        let budget = charter_tokens + 20;

        let thread = test_thread("Tight", budget);
        let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());

        // Large outputs to force truncation.
        let big_summary = "x".repeat(500);
        let outputs = vec![
            make_output(thread.thread_id, &big_summary),
            make_output(thread.thread_id, &big_summary),
        ];

        let ctx =
            compile_thread_context(&counter, &thread, &snapshot, &outputs, TickId::new(), None)
                .unwrap();

        // Charter must be preserved.
        assert!(
            ctx.prompt.contains(charter_text),
            "Charter should be preserved even under tight budget"
        );
        // Outputs section should have been truncated.
        assert!(
            ctx.truncated_sections
                .contains(&"recent_outputs".to_string()),
            "Outputs should be in truncated_sections"
        );
        assert!(
            ctx.total_tokens <= budget,
            "total_tokens={} exceeds budget={budget}",
            ctx.total_tokens
        );
    }

    #[test]
    fn context_handles_empty_outputs() {
        let counter = ApproximateTokenCounter;
        let thread = test_thread("EmptyOut", 5000);
        let snapshot = StateSnapshot::initial(VesselId::new(), "test mission".into());

        let ctx =
            compile_thread_context(&counter, &thread, &snapshot, &[], TickId::new(), None).unwrap();

        assert!(!ctx.prompt.is_empty(), "Prompt should not be empty");
        assert!(
            !ctx.prompt.contains("YOUR RECENT OUTPUTS"),
            "Should not contain outputs header when there are none"
        );
        assert!(
            ctx.total_tokens <= thread.token_budget,
            "total_tokens={} exceeds budget={}",
            ctx.total_tokens,
            thread.token_budget
        );
    }
}
