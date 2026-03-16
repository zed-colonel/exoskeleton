//! Context Compiler — assembles token-budgeted prompts from multiple data sources.
//!
//! The ContextCompiler is the centerpiece of Sprint 3 and the backbone of I5
//! (context compiled, not accumulated). Each tick, the master loop (Sprint 5)
//! calls `compile()` with fresh data from stores. The compiler never carries
//! forward state between calls — it is purely functional.

use std::sync::Arc;

use exoskeleton_core::{
    EpisodicSummary, EventEntry, ExoError, LongTermNote, RelationshipSnapshot, StateSnapshot,
    ThreadContribution, VesselId,
};
use serde::{Deserialize, Serialize};

use crate::render;
use crate::tokens::TokenCounter;

/// Input data for context compilation.
///
/// All fields are references to data fetched by the caller (master loop).
/// The compiler does not query stores directly — it is a pure transformation
/// from sources to compiled text.
pub struct ContextSources<'a> {
    /// Vessel identity for the system section.
    pub vessel_id: VesselId,
    /// Vessel's mission statement.
    pub mission: &'a str,
    /// Current state snapshot (from SnapshotStore).
    pub snapshot: &'a StateSnapshot,
    /// Compiled relationship summary (from Align step, Sprint 8).
    /// `None` if no relationship data exists yet.
    pub relationship_snapshot: Option<&'a RelationshipSnapshot>,
    /// Latest thread contributions (from ThreadRegistry, Sprint 6).
    pub thread_contributions: &'a [ThreadContribution],
    /// Recent events (from EventLedger).
    pub recent_events: &'a [EventEntry],
    /// Episodic memory summaries (from MemoryStore).
    pub episodic_summaries: &'a [EpisodicSummary],
    /// Long-term memory notes (from MemoryStore).
    pub long_term_notes: &'a [LongTermNote],
    /// Current working context / task focus.
    pub working_context: &'a str,
    /// Pre-resolved system section template (Epoch 0).
    /// If Some, used instead of the hardcoded `render_system_section()` output.
    /// The host layer resolves the template and passes the result as data.
    pub system_section_override: Option<&'a str>,
}

/// Result of context compilation.
///
/// Contains the assembled prompt text, total token count, per-section
/// breakdown, and information about which sections were truncated.
#[derive(Debug, Clone)]
pub struct CompiledContext {
    /// The assembled prompt text, ready for LLM consumption.
    pub prompt: String,
    /// Actual token count of the compiled prompt.
    pub total_tokens: u64,
    /// Token budget that was targeted.
    pub budget: u64,
    /// Per-section token breakdown.
    pub sections: Vec<SectionResult>,
    /// Names of sections that were truncated to fit budget.
    pub truncated_sections: Vec<String>,
}

/// Token allocation result for one section.
#[derive(Debug, Clone)]
pub struct SectionResult {
    /// Section name (e.g., "system", "state_snapshot").
    pub name: String,
    /// Tokens allocated to this section.
    pub allocated: u64,
    /// Actual tokens used (after rendering and possible truncation).
    pub used: u64,
    /// Whether this section was truncated to fit its allocation.
    pub truncated: bool,
}

/// Priority level for context sections.
///
/// Determines truncation order when the total context exceeds the token budget.
/// Critical sections are never truncated; Low sections are truncated first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPriority {
    /// First to be truncated when budget is tight.
    Low,
    /// Truncated before High sections.
    Medium,
    /// Truncated only when necessary.
    High,
    /// Never truncated. If budget cannot accommodate Critical sections,
    /// compilation returns an error.
    Critical,
}

/// Budget allocation configuration for one section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionAllocation {
    /// Priority level — determines truncation order.
    pub priority: SectionPriority,
    /// Target percentage of total budget (0.0 – 1.0).
    /// Actual allocation may be less (if section content is smaller)
    /// or more (if surplus budget is redistributed from smaller sections).
    pub target_pct: f64,
}

/// Configuration for all context sections.
///
/// Controls priority and budget allocation for each section in the compiled
/// context. Default values are tuned for a general-purpose agent with
/// balanced attention across all data sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionPriorities {
    /// Vessel identity, mission, and operating instructions.
    pub system: SectionAllocation,
    /// Current StateSnapshot rendered as text.
    pub state_snapshot: SectionAllocation,
    /// Compiled relationship summary.
    pub relationship_snapshot: SectionAllocation,
    /// Latest cognitive thread outputs.
    pub thread_outputs: SectionAllocation,
    /// Recent events from the EventLedger.
    pub recent_events: SectionAllocation,
    /// Episodic memory summaries.
    pub episodic_memory: SectionAllocation,
    /// Long-term memory notes.
    pub long_term_memory: SectionAllocation,
    /// Current task-specific working context.
    pub working_context: SectionAllocation,
}

impl Default for SectionPriorities {
    fn default() -> Self {
        Self {
            system: SectionAllocation {
                priority: SectionPriority::Critical,
                target_pct: 0.10,
            },
            state_snapshot: SectionAllocation {
                priority: SectionPriority::Critical,
                target_pct: 0.15,
            },
            relationship_snapshot: SectionAllocation {
                priority: SectionPriority::High,
                target_pct: 0.10,
            },
            thread_outputs: SectionAllocation {
                priority: SectionPriority::Medium,
                target_pct: 0.15,
            },
            recent_events: SectionAllocation {
                priority: SectionPriority::Medium,
                target_pct: 0.15,
            },
            episodic_memory: SectionAllocation {
                priority: SectionPriority::Medium,
                target_pct: 0.15,
            },
            long_term_memory: SectionAllocation {
                priority: SectionPriority::Low,
                target_pct: 0.10,
            },
            working_context: SectionAllocation {
                priority: SectionPriority::Medium,
                target_pct: 0.10,
            },
        }
    }
}

impl SectionPriorities {
    /// Return all section allocations as (name, allocation) pairs in fixed render order.
    fn as_ordered_pairs(&self) -> Vec<(&'static str, &SectionAllocation)> {
        vec![
            ("system", &self.system),
            ("state_snapshot", &self.state_snapshot),
            ("relationship_snapshot", &self.relationship_snapshot),
            ("working_context", &self.working_context),
            ("thread_outputs", &self.thread_outputs),
            ("recent_events", &self.recent_events),
            ("episodic_memory", &self.episodic_memory),
            ("long_term_memory", &self.long_term_memory),
        ]
    }
}

/// The context compilation engine.
///
/// Assembles a token-budgeted prompt from multiple data sources (I5: context
/// compiled, not accumulated). Each tick, the master loop calls `compile()`
/// with fresh data from stores. The compiler never carries forward state
/// between calls — it is purely functional.
pub struct ContextCompiler {
    token_counter: Arc<dyn TokenCounter>,
    total_budget: u64,
    section_priorities: SectionPriorities,
}

impl ContextCompiler {
    /// Create a new ContextCompiler with the given configuration.
    pub fn new(
        token_counter: Arc<dyn TokenCounter>,
        total_budget: u64,
        section_priorities: SectionPriorities,
    ) -> Self {
        // H-9: Warn if target percentages don't sum to ~1.0
        let total_pct: f64 = section_priorities
            .as_ordered_pairs()
            .iter()
            .map(|(_, a)| a.target_pct)
            .sum();
        if (total_pct - 1.0).abs() > 0.01 {
            tracing::warn!(
                total_pct,
                "SectionPriorities target percentages sum to {total_pct:.3}, expected ~1.0"
            );
        }
        Self {
            token_counter,
            total_budget,
            section_priorities,
        }
    }

    /// Create a ContextCompiler with default section priorities.
    pub fn with_defaults(token_counter: Arc<dyn TokenCounter>, total_budget: u64) -> Self {
        Self::new(token_counter, total_budget, SectionPriorities::default())
    }

    /// Compile context from the given sources.
    ///
    /// The compilation algorithm:
    /// 1. Render each section to text using section renderers
    /// 2. Count tokens for each rendered section
    /// 3. Compute target allocation per section (total_budget × target_pct)
    /// 4. For sections whose rendered text fits within target: surplus is freed
    /// 5. Redistribute surplus proportionally to sections that need more
    /// 6. If still over budget: truncate sections from lowest priority first
    /// 7. Assemble final prompt in fixed section order
    ///
    /// Returns `ExoError::ContextCompilation` only if Critical sections cannot
    /// fit within the total budget (a configuration error, not a data error).
    pub fn compile(&self, sources: &ContextSources<'_>) -> Result<CompiledContext, ExoError> {
        // Phase 1: Render each section
        let rendered = self.render_sections(sources);

        // Phase 2: Measure token counts
        let measured: Vec<(&str, String, u64, SectionPriority)> = rendered
            .into_iter()
            .map(|(name, text, priority)| {
                let tokens = if text.is_empty() {
                    0
                } else {
                    self.token_counter.count(&text)
                };
                (name, text, tokens, priority)
            })
            .collect();

        // Check if Critical sections alone exceed budget
        let critical_total: u64 = measured
            .iter()
            .filter(|(_, text, _, priority)| {
                !text.is_empty() && *priority == SectionPriority::Critical
            })
            .map(|(_, _, tokens, _)| *tokens)
            .sum();

        if critical_total > self.total_budget {
            return Err(ExoError::ContextCompilation(format!(
                "Critical sections require {critical_total} tokens but budget is only {}",
                self.total_budget
            )));
        }

        // Phase 3: Allocate
        // Reserve tokens for newline separators between populated sections
        let populated_count = measured
            .iter()
            .filter(|(_, text, _, _)| !text.is_empty())
            .count() as u64;
        // Conservative: 1 token per separator (newline between sections)
        let separator_reserve = populated_count.saturating_sub(1);
        let effective_budget = self.total_budget.saturating_sub(separator_reserve);

        let pairs = self.section_priorities.as_ordered_pairs();
        let mut allocations: Vec<SectionState> = measured
            .iter()
            .zip(pairs.iter())
            .map(|((name, text, actual_tokens, priority), (_, alloc))| {
                let target = (effective_budget as f64 * alloc.target_pct) as u64;
                SectionState {
                    name: name.to_string(),
                    text: text.clone(),
                    actual_tokens: *actual_tokens,
                    target,
                    allocated: 0,
                    priority: *priority,
                    truncated: false,
                }
            })
            .collect();

        // For empty sections, allocate 0 and collect surplus
        let mut surplus: u64 = 0;
        let mut needs_more_indices: Vec<usize> = Vec::new();

        for (i, section) in allocations.iter_mut().enumerate() {
            if section.actual_tokens == 0 {
                // Empty section — all target budget is surplus
                surplus += section.target;
                section.allocated = 0;
            } else if section.actual_tokens <= section.target {
                // Fits within target — allocate only what's needed
                section.allocated = section.actual_tokens;
                surplus += section.target - section.actual_tokens;
            } else {
                // Needs more than target
                section.allocated = section.target;
                needs_more_indices.push(i);
            }
        }

        // Phase 4: Redistribute surplus (highest priority gets surplus first)
        needs_more_indices.sort_by(|a, b| {
            allocations[*b]
                .priority
                .cmp(&allocations[*a].priority)
                .then_with(|| a.cmp(b))
        });

        for &idx in &needs_more_indices {
            let section = &mut allocations[idx];
            let additional = (section.actual_tokens - section.target).min(surplus);
            section.allocated += additional;
            surplus -= additional;
        }

        // Phase 5: Truncate if over effective budget
        let total_allocated: u64 = allocations.iter().map(|s| s.allocated).sum();
        if total_allocated > effective_budget {
            // Sort over-budget sections by priority ASC (lowest truncated first)
            let mut truncation_order: Vec<usize> = (0..allocations.len())
                .filter(|i| allocations[*i].allocated > 0)
                .collect();
            truncation_order.sort_by(|a, b| {
                allocations[*a]
                    .priority
                    .cmp(&allocations[*b].priority)
                    .then_with(|| b.cmp(a))
            });

            let mut current_total: u64 = total_allocated;
            for &idx in &truncation_order {
                if current_total <= effective_budget {
                    break;
                }
                // Don't truncate Critical sections
                if allocations[idx].priority == SectionPriority::Critical {
                    continue;
                }
                let excess = current_total - effective_budget;
                let reduce = excess.min(allocations[idx].allocated);
                allocations[idx].allocated -= reduce;
                current_total -= reduce;
                if allocations[idx].allocated < allocations[idx].actual_tokens {
                    allocations[idx].truncated = true;
                }
            }
        }

        // Phase 6: Assemble — truncate text where needed and build final prompt
        let mut prompt = String::new();
        let mut section_results = Vec::new();
        let mut truncated_sections = Vec::new();

        for section in &mut allocations {
            if section.allocated == 0 || section.text.is_empty() {
                section_results.push(SectionResult {
                    name: section.name.clone(),
                    allocated: section.allocated,
                    used: 0,
                    truncated: false,
                });
                continue;
            }

            let final_text = if section.truncated || section.actual_tokens > section.allocated {
                let truncated = self
                    .token_counter
                    .truncate_to_budget(&section.text, section.allocated);
                section.truncated = true;
                truncated
            } else {
                section.text.clone()
            };

            let used = self.token_counter.count(&final_text);

            if section.truncated {
                truncated_sections.push(section.name.clone());
            }

            if !final_text.is_empty() {
                if !prompt.is_empty() {
                    prompt.push('\n');
                }
                prompt.push_str(&final_text);
            }

            section_results.push(SectionResult {
                name: section.name.clone(),
                allocated: section.allocated,
                used,
                truncated: section.truncated,
            });
        }

        // Final safety: guarantee total_tokens <= budget even with rounding
        let mut total_tokens = self.token_counter.count(&prompt);
        if total_tokens > self.total_budget {
            prompt = self
                .token_counter
                .truncate_to_budget(&prompt, self.total_budget);
            total_tokens = self.token_counter.count(&prompt);
        }

        Ok(CompiledContext {
            prompt,
            total_tokens,
            budget: self.total_budget,
            sections: section_results,
            truncated_sections,
        })
    }

    /// Render all sections to text, returning (name, text, priority) triples
    /// in the fixed section order.
    fn render_sections(
        &self,
        sources: &ContextSources<'_>,
    ) -> Vec<(&'static str, String, SectionPriority)> {
        let p = &self.section_priorities;
        vec![
            (
                "system",
                match sources.system_section_override {
                    Some(override_text) => format!("=== SYSTEM ===\n{override_text}"),
                    None => render::render_system_section(sources.vessel_id, sources.mission),
                },
                p.system.priority,
            ),
            (
                "state_snapshot",
                render::render_snapshot_section(sources.snapshot),
                p.state_snapshot.priority,
            ),
            (
                "relationship_snapshot",
                sources
                    .relationship_snapshot
                    .map(render::render_relationship_section)
                    .unwrap_or_default(),
                p.relationship_snapshot.priority,
            ),
            (
                "working_context",
                render::render_working_context(sources.working_context),
                p.working_context.priority,
            ),
            (
                "thread_outputs",
                render::render_thread_outputs(sources.thread_contributions),
                p.thread_outputs.priority,
            ),
            (
                "recent_events",
                render::render_recent_events(sources.recent_events),
                p.recent_events.priority,
            ),
            (
                "episodic_memory",
                render::render_episodic_memory(sources.episodic_summaries),
                p.episodic_memory.priority,
            ),
            (
                "long_term_memory",
                render::render_long_term_memory(sources.long_term_notes),
                p.long_term_memory.priority,
            ),
        ]
    }
}

/// Internal state for budget allocation tracking.
struct SectionState {
    name: String,
    text: String,
    actual_tokens: u64,
    target: u64,
    allocated: u64,
    priority: SectionPriority,
    truncated: bool,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{
        ArtifactId, BudgetStatus, EventType, LedgerEntryId, PrincipalId, PrincipalSummary,
        ThreadId, ThreadStatus, ThreadSummary, VesselStatus,
    };

    use super::*;
    use crate::tokens::ApproximateTokenCounter;

    fn make_compiler(budget: u64) -> ContextCompiler {
        ContextCompiler::with_defaults(Arc::new(ApproximateTokenCounter), budget)
    }

    fn make_snapshot() -> StateSnapshot {
        let mut snap = StateSnapshot::initial(VesselId::new(), "Test mission".into());
        snap.tick_number = 5;
        snap.status = VesselStatus::Thinking;
        snap.working_context = "Evaluating options".into();
        snap.budget_status = BudgetStatus {
            local_tokens_remaining: 40_000,
            frontier_tokens_remaining: 10_000,
            frontier_cost_cents_remaining: 200,
            time_secs_remaining: 1800,
            thrash_level: exoskeleton_core::budget::ThrashLevel::None,
            tool_invocations_remaining: 950,
        };
        snap
    }

    fn make_full_sources(snapshot: &StateSnapshot) -> ContextSources<'_> {
        ContextSources {
            vessel_id: snapshot.vessel_id,
            mission: &snapshot.mission,
            snapshot,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: &snapshot.working_context,
            system_section_override: None,
        }
    }

    // ── T-5: Context Compiler — Budget Allocation ──

    #[test]
    fn compile_generous_budget() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();
        let sources = make_full_sources(&snap);
        let result = compiler.compile(&sources).unwrap();

        assert!(result.total_tokens <= 100_000);
        assert!(result.truncated_sections.is_empty());
        // System and state sections always present
        assert!(result.prompt.contains("=== SYSTEM ==="));
        assert!(result.prompt.contains("=== STATE"));
    }

    #[test]
    fn compile_tight_budget() {
        let compiler = make_compiler(500);
        let snap = make_snapshot();

        let episodic = vec![EpisodicSummary {
            id: ArtifactId::from_content(b"ep"),
            start_tick: 0,
            end_tick: 4,
            summary: "Long summary text ".repeat(50),
            key_events: vec!["event1".into()],
            token_count: 100,
            created_at: Utc::now(),
        }];
        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"lt"),
            topic: "test".into(),
            content: "Long content text ".repeat(50),
            tags: vec!["tag".into()],
            token_count: 100,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &episodic,
            long_term_notes: &notes,
            working_context: &snap.working_context,
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.total_tokens <= 500);
    }

    #[test]
    fn compile_minimal_budget() {
        // Budget just large enough for critical sections (~125 tokens) but not much else
        let compiler = make_compiler(150);
        let snap = make_snapshot();

        let episodic = vec![EpisodicSummary {
            id: ArtifactId::from_content(b"ep"),
            start_tick: 0,
            end_tick: 4,
            summary: "A ".repeat(200),
            key_events: Vec::new(),
            token_count: 100,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &episodic,
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.total_tokens <= 150);
        // Critical sections (system, state) should be present
        assert!(result.prompt.contains("=== SYSTEM ==="));
    }

    #[test]
    fn compile_empty_sources() {
        let compiler = make_compiler(10_000);
        let snap = StateSnapshot::initial(VesselId::new(), "Test".into());
        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.total_tokens <= 10_000);
        assert!(result.prompt.contains("=== SYSTEM ==="));
        assert!(result.truncated_sections.is_empty());
    }

    #[test]
    fn compile_section_order() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();

        let rel_snap = RelationshipSnapshot {
            principals: vec![PrincipalSummary {
                principal_id: PrincipalId::new(),
                display_name: "Keith".into(),
                role: "operator".into(),
                trust_level: 0.9,
                active_commitments: 1,
                last_interaction: None,
                notes: None,
            }],
            compiled_at: Utc::now(),
        };

        let contributions = vec![ThreadContribution {
            thread_id: ThreadId::new(),
            artifact_id: ArtifactId::from_content(b"tc"),
            summary: "Thread analysis complete".into(),
        }];

        let events = vec![EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::TickCompleted,
            payload_ref: None,
            summary: "Tick 4 done".into(),
            timestamp: Utc::now(),
        }];

        let episodic = vec![EpisodicSummary {
            id: ArtifactId::from_content(b"ep"),
            start_tick: 0,
            end_tick: 3,
            summary: "Initial phase".into(),
            key_events: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        }];

        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"lt"),
            topic: "arch".into(),
            content: "Dual AQ architecture".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: Some(&rel_snap),
            thread_contributions: &contributions,
            recent_events: &events,
            episodic_summaries: &episodic,
            long_term_notes: &notes,
            working_context: "Current focus",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        let prompt = &result.prompt;

        // Verify fixed order: system, state, relationship, working_context,
        // threads, events, episodic, long_term
        let sys_pos = prompt.find("=== SYSTEM ===").unwrap();
        let state_pos = prompt.find("=== STATE").unwrap();
        let rel_pos = prompt.find("=== RELATIONSHIPS ===").unwrap();
        let wc_pos = prompt.find("=== WORKING CONTEXT ===").unwrap();
        let thread_pos = prompt.find("=== THREAD OUTPUTS ===").unwrap();
        let events_pos = prompt.find("=== RECENT EVENTS ===").unwrap();
        let ep_pos = prompt.find("=== EPISODIC MEMORY ===").unwrap();
        let lt_pos = prompt.find("=== LONG-TERM MEMORY ===").unwrap();

        assert!(sys_pos < state_pos);
        assert!(state_pos < rel_pos);
        assert!(rel_pos < wc_pos);
        assert!(wc_pos < thread_pos);
        assert!(thread_pos < events_pos);
        assert!(events_pos < ep_pos);
        assert!(ep_pos < lt_pos);
    }

    #[test]
    fn compile_surplus_redistribution() {
        // System section is small; recent_events section is large.
        // recent_events should get surplus from system.
        let compiler = make_compiler(500);
        let snap = make_snapshot();

        let events: Vec<EventEntry> = (0..20)
            .map(|i| EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::TickCompleted,
                payload_ref: None,
                summary: format!("Event number {i} with some description text"),
                timestamp: Utc::now(),
            })
            .collect();

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &events,
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.total_tokens <= 500);

        // recent_events section should have gotten some content
        let events_section = result.sections.iter().find(|s| s.name == "recent_events");
        assert!(events_section.is_some());
        assert!(events_section.unwrap().used > 0);
    }

    #[test]
    fn compile_respects_total_budget() {
        let compiler = make_compiler(300);
        let snap = make_snapshot();

        let episodic: Vec<EpisodicSummary> = (0..10)
            .map(|i| EpisodicSummary {
                id: ArtifactId::from_content(format!("ep{i}").as_bytes()),
                start_tick: i * 5,
                end_tick: i * 5 + 4,
                summary: format!("Summary for span {i} with details"),
                key_events: vec!["event".into()],
                token_count: 20,
                created_at: Utc::now(),
            })
            .collect();

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &episodic,
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(
            result.total_tokens <= 300,
            "total_tokens={} exceeds budget=300",
            result.total_tokens
        );
    }

    #[test]
    fn compile_reports_truncation() {
        let compiler = make_compiler(300);
        let snap = make_snapshot();

        let notes: Vec<LongTermNote> = (0..20)
            .map(|i| LongTermNote {
                id: ArtifactId::from_content(format!("lt{i}").as_bytes()),
                topic: format!("topic_{i}"),
                content: "A detailed note with significant content ".repeat(5),
                tags: Vec::new(),
                token_count: 50,
                created_at: Utc::now(),
            })
            .collect();

        let episodic: Vec<EpisodicSummary> = (0..10)
            .map(|i| EpisodicSummary {
                id: ArtifactId::from_content(format!("ep{i}").as_bytes()),
                start_tick: i * 5,
                end_tick: i * 5 + 4,
                summary: "Detailed summary of events ".repeat(5),
                key_events: vec!["something happened".into()],
                token_count: 30,
                created_at: Utc::now(),
            })
            .collect();

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &episodic,
            long_term_notes: &notes,
            working_context: "Some focus text",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.total_tokens <= 300);
        // With a budget this tight, some sections should be truncated
        assert!(
            !result.truncated_sections.is_empty(),
            "Expected some sections to be truncated"
        );
    }

    #[test]
    fn compile_section_metadata() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();
        let sources = make_full_sources(&snap);
        let result = compiler.compile(&sources).unwrap();

        // Should have entries for all 8 sections
        assert_eq!(result.sections.len(), 8);
        let names: Vec<&str> = result.sections.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"system"));
        assert!(names.contains(&"state_snapshot"));
        assert!(names.contains(&"relationship_snapshot"));
        assert!(names.contains(&"working_context"));
        assert!(names.contains(&"thread_outputs"));
        assert!(names.contains(&"recent_events"));
        assert!(names.contains(&"episodic_memory"));
        assert!(names.contains(&"long_term_memory"));
    }

    #[test]
    fn compile_critical_sections_preserved() {
        // Budget just fits critical sections (~125 tokens). Low-priority content truncated.
        let compiler = make_compiler(150);
        let snap = make_snapshot();

        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"lt"),
            topic: "test".into(),
            content: "Very long content ".repeat(100),
            tags: Vec::new(),
            token_count: 500,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &notes,
            working_context: "",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.prompt.contains("=== SYSTEM ==="));
        assert!(result.prompt.contains("=== STATE"));
        assert!(result.total_tokens <= 150);
    }

    #[test]
    fn compile_budget_too_small_for_critical() {
        let compiler = make_compiler(1);
        let snap = make_snapshot();
        let sources = make_full_sources(&snap);
        let result = compiler.compile(&sources);
        assert!(
            result.is_err(),
            "Expected ContextCompilation error for budget=1"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExoError::ContextCompilation(_)),
            "Expected ContextCompilation error, got: {err}"
        );
    }

    #[test]
    fn compile_no_relationship_data() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();
        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };
        let result = compiler.compile(&sources).unwrap();
        assert!(!result.prompt.contains("RELATIONSHIPS"));
        let rel_section = result
            .sections
            .iter()
            .find(|s| s.name == "relationship_snapshot")
            .unwrap();
        assert_eq!(rel_section.used, 0);
    }

    #[test]
    fn compile_no_thread_data() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();
        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };
        let result = compiler.compile(&sources).unwrap();
        assert!(!result.prompt.contains("THREAD OUTPUTS"));
    }

    // ── T-6: Context Compiler — Integration ──

    #[test]
    fn compile_realistic_scenario() {
        let compiler = make_compiler(10_000);
        let mut snap = make_snapshot();
        snap.thread_summaries = vec![ThreadSummary {
            thread_id: ThreadId::new(),
            name: "Monitor".into(),
            status: ThreadStatus::Active,
            last_output_summary: Some("All clear".into()),
            token_budget_remaining: 5000,
        }];

        let events = vec![
            EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::VesselStarted,
                payload_ref: None,
                summary: "Boot complete".into(),
                timestamp: Utc::now(),
            },
            EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::TickCompleted,
                payload_ref: None,
                summary: "Tick 4 done".into(),
                timestamp: Utc::now(),
            },
            EventEntry {
                id: LedgerEntryId::new(),
                tick_id: None,
                event_type: EventType::ActionExecuted,
                payload_ref: None,
                summary: "Wrote analysis.md".into(),
                timestamp: Utc::now(),
            },
        ];

        let episodic = vec![
            EpisodicSummary {
                id: ArtifactId::from_content(b"ep1"),
                start_tick: 0,
                end_tick: 2,
                summary: "Initial exploration".into(),
                key_events: vec!["Discovered project structure".into()],
                token_count: 15,
                created_at: Utc::now(),
            },
            EpisodicSummary {
                id: ArtifactId::from_content(b"ep2"),
                start_tick: 3,
                end_tick: 4,
                summary: "Analysis phase".into(),
                key_events: Vec::new(),
                token_count: 10,
                created_at: Utc::now(),
            },
        ];

        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"lt1"),
            topic: "architecture".into(),
            content: "Dual AQ design is a sacred invariant".into(),
            tags: vec!["I9".into()],
            token_count: 15,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &events,
            episodic_summaries: &episodic,
            long_term_notes: &notes,
            working_context: "Evaluating options",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(result.total_tokens <= 10_000);
        assert!(result.prompt.contains("=== SYSTEM ==="));
        assert!(result.prompt.contains("=== STATE"));
        assert!(result.prompt.contains("=== RECENT EVENTS ==="));
        assert!(result.prompt.contains("=== EPISODIC MEMORY ==="));
        assert!(result.prompt.contains("=== LONG-TERM MEMORY ==="));
        assert!(result.prompt.contains("=== WORKING CONTEXT ==="));
    }

    #[test]
    fn compile_idempotent() {
        let compiler = make_compiler(10_000);
        let snap = make_snapshot();
        let sources = make_full_sources(&snap);

        let result1 = compiler.compile(&sources).unwrap();
        let result2 = compiler.compile(&sources).unwrap();

        assert_eq!(result1.prompt, result2.prompt);
        assert_eq!(result1.total_tokens, result2.total_tokens);
        assert_eq!(result1.sections.len(), result2.sections.len());
    }

    #[test]
    fn compile_with_all_sources() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();

        let rel_snap = RelationshipSnapshot {
            principals: vec![PrincipalSummary {
                principal_id: PrincipalId::new(),
                display_name: "Keith".into(),
                role: "operator".into(),
                trust_level: 0.9,
                active_commitments: 1,
                last_interaction: None,
                notes: None,
            }],
            compiled_at: Utc::now(),
        };

        let contributions = vec![ThreadContribution {
            thread_id: ThreadId::new(),
            artifact_id: ArtifactId::from_content(b"tc"),
            summary: "Analysis complete".into(),
        }];

        let events = vec![EventEntry {
            id: LedgerEntryId::new(),
            tick_id: None,
            event_type: EventType::TickCompleted,
            payload_ref: None,
            summary: "Done".into(),
            timestamp: Utc::now(),
        }];

        let episodic = vec![EpisodicSummary {
            id: ArtifactId::from_content(b"ep"),
            start_tick: 0,
            end_tick: 3,
            summary: "Early phase".into(),
            key_events: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        }];

        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"lt"),
            topic: "design".into(),
            content: "Important note".into(),
            tags: Vec::new(),
            token_count: 10,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: Some(&rel_snap),
            thread_contributions: &contributions,
            recent_events: &events,
            episodic_summaries: &episodic,
            long_term_notes: &notes,
            working_context: "Current task",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        // All 8 section headers should be present
        assert!(result.prompt.contains("=== SYSTEM ==="));
        assert!(result.prompt.contains("=== STATE"));
        assert!(result.prompt.contains("=== RELATIONSHIPS ==="));
        assert!(result.prompt.contains("=== WORKING CONTEXT ==="));
        assert!(result.prompt.contains("=== THREAD OUTPUTS ==="));
        assert!(result.prompt.contains("=== RECENT EVENTS ==="));
        assert!(result.prompt.contains("=== EPISODIC MEMORY ==="));
        assert!(result.prompt.contains("=== LONG-TERM MEMORY ==="));
    }

    #[test]
    fn compile_incremental_sources() {
        let compiler = make_compiler(100_000);
        let snap = make_snapshot();

        // First: only system and state
        let sources1 = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &[],
            working_context: "",
            system_section_override: None,
        };
        let result1 = compiler.compile(&sources1).unwrap();

        // Second: add more sources
        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"lt"),
            topic: "test".into(),
            content: "A note".into(),
            tags: Vec::new(),
            token_count: 5,
            created_at: Utc::now(),
        }];
        let sources2 = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &notes,
            working_context: "New focus",
            system_section_override: None,
        };
        let result2 = compiler.compile(&sources2).unwrap();

        // Both succeed, second has more content
        assert!(result2.total_tokens >= result1.total_tokens);
        assert!(result2.prompt.contains("LONG-TERM MEMORY"));
        assert!(!result1.prompt.contains("LONG-TERM MEMORY"));
    }

    // ── T-9: Edge Case — Compiler ──

    #[test]
    fn compiler_with_very_long_section() {
        let compiler = make_compiler(1000);
        let snap = make_snapshot();

        let notes = vec![LongTermNote {
            id: ArtifactId::from_content(b"huge"),
            topic: "test".into(),
            content: "word ".repeat(10_000), // ~50K tokens
            tags: Vec::new(),
            token_count: 12500,
            created_at: Utc::now(),
        }];

        let sources = ContextSources {
            vessel_id: snap.vessel_id,
            mission: &snap.mission,
            snapshot: &snap,
            relationship_snapshot: None,
            thread_contributions: &[],
            recent_events: &[],
            episodic_summaries: &[],
            long_term_notes: &notes,
            working_context: "",
            system_section_override: None,
        };

        let result = compiler.compile(&sources).unwrap();
        assert!(
            result.total_tokens <= 1000,
            "total_tokens={} exceeds budget=1000",
            result.total_tokens
        );
        // Should not panic; should truncate cleanly
        assert!(result.prompt.contains("=== SYSTEM ==="));
    }
}
