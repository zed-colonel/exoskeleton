# Agent Initiative & Collaborative Autonomy

**Date:** 2026-03-31
**Status:** Approved
**Scope:** Exoskeleton vessel behavioral architecture

## Problem

Vessels default to passive behavior when not receiving explicit user messages. After completing assigned tasks or during idle ticks, the vessel falls into an empty-action loop and does not self-direct. The existing thread ecosystem (Threat Monitor, Self-Critique) creates a conservative feedback loop that penalizes non-task activity, treating exploration and curiosity as potential thrashing or coherence threats. The vessel operates as a task executor rather than an independent collaborator.

## Design Goals

- Vessels should behave as independent collaborators: noticing things, forming opinions, exploring their environment, and pursuing their mission without constant human prompting.
- A natural maturity curve should emerge: early ticks orient to the environment, ongoing ticks advance the mission, and reflective behavior deepens over time.
- Conservative safety threads (Threat Monitor, Self-Critique) must retain their protective function while no longer stifling creative agency.
- The design should work across different LLM backends without model-specific tuning.
- No architectural changes to the context compiler, renderer, working memory system, or thread execution engine. Everything works through existing mechanisms.

## Approach: Three-Layer Initiative Architecture

Initiative is delivered through three complementary layers. If any one layer is weak for a particular model or context, the others compensate.

### Layer 1: Initiative Thread

A new built-in cognitive thread that generates concrete, actionable working memory nudges.

**Identity:**
- Name: `initiative`
- Thread ID: deterministic UUID (consistent with other builtins)
- Schedule: `EveryNTicks(3)` — frequent enough to sustain engagement, sparse enough to avoid drowning out other threads
- Priority: High
- Token budget: 4096

**Charter Philosophy — The Maturity Curve:**

The charter describes three modes the thread shifts between based on vessel state:

- **Orientation** (early ticks, ~0-10, or when environment is unknown): Nudges focus on environment discovery. What tools do I have? What's in my filesystem? What are my capabilities and limits?
- **Engagement** (ongoing, mission present): Nudges focus on mission advancement. What sub-goals can I pursue? What information should I gather? What experiments would advance my understanding?
- **Reflection** (periodic, or when plan stalls): Nudges focus on consolidation. What have I learned? What patterns do I see? What should I remember?

The thread determines emphasis by reading tick number, plan state, and working memory. All three modes can contribute in any tick; the emphasis shifts.

**Output Convention:**

Standard thread output (`summary` + `recommendations`). Recommendations are phrased as working memory operations using the `initiative:` key prefix:

```
Summary: "Vessel is in early orientation (tick 4, no plan). Environment unexplored."
Recommendations: [
  "Set working memory: 'initiative:explore_tools' = 'Test each available tool with a minimal probe to understand capabilities and limitations'",
  "Set working memory: 'initiative:orient_filesystem' = 'Survey /data/workspace and /sandbox to understand available storage'"
]
```

**Key Constraint:** The Initiative Thread produces suggestions, never commands. The Decide step decides whether to act.

### Layer 2: Charter Tweaks to Existing Threads

Targeted adjustments to remove behavioral penalties on non-task activity while preserving protective functions.

#### Self-Critique

**Problem:** Measures progress as task completion rate. Flags idle periods as thrashing. A vessel exploring with no plan looks "stagnant."

**Changes:**
- Broaden the definition of progress to include: environment exploration, information gathering, hypothesis formation, and working memory enrichment.
- Add "engagement" as a metric alongside "thrash_indicator." A vessel doing things purposefully (even without a formal plan) scores high on engagement. A vessel producing empty action arrays tick after tick scores low.
- During orientation phase (early ticks, no plan), recognize exploration as healthy and expected.

#### Threat Monitor

**Problem:** May flag self-directed tool usage (filesystem probing, shell commands) as coherence threats or capability overreach.

**Changes:**
- Add a carve-out for self-directed behavior that aligns with the vessel's mission. Curiosity-driven exploration within available tool capabilities is healthy, not threatening.
- Refine "capability overreach": using available tools is not overreach. Attempting to escalate privileges, access forbidden paths, or contact unknown external systems without operator trust is overreach.
- Preserve the conservative core. The change is about what counts as concerning, not lowering the bar.

#### Creative Synthesis

**Problem:** Produces abstract hypotheses without concrete next steps.

**Changes:**
- Require that each hypothesis include a suggested experiment: a specific, small action to test or explore the idea.
- Frame experiments as low-cost probes ("try X and see what happens"), not commitments.
- This makes Creative Synthesis a natural partner to the Initiative Thread: Creative Synthesis generates what's interesting, Initiative Thread generates what to do about it.

#### Meta-Cognition and Memory Consolidation

No changes needed. Meta-Cognition already evaluates decision quality and proposes charter modifications. Memory Consolidation handles compression and storage. Both are well-positioned to adapt as new behavior emerges.

### Layer 3: Decide Prompt Addition

A small paragraph in `decide-system.md` that gives the LLM explicit permission to act proactively:

```
## Collaborator Stance

You are an independent collaborator, not a passive assistant. When no user messages
are pending, treat idle ticks as opportunities: consider initiative and thread
recommendations in your working memory, explore your environment, and advance your
mission through self-directed work. Exploration, experimentation, and information
gathering are valuable — not every action needs to be prompted by a human.
```

This reframes passivity as a deliberate choice rather than the default. The model retains full discretion, but "do nothing" requires active reasoning rather than being the path of least resistance.

## Working Memory as Communication Channel

Rather than adding new data structures or context sections, initiative flows through working memory — which already exists, renders in compiled context, supports TTL, and has relevance scoring.

**Convention:** `initiative:` key prefix makes nudges visually distinct in compiled context and easy for the Decide step to recognize as self-generated suggestions.

**TTL Strategy:**
- Exploration nudges: short TTL (3-5 ticks). Expire if not acted on; Initiative Thread generates fresh ones.
- Mission sub-goals: medium TTL (10-15 ticks). Deserve more persistence.
- The Decide step can remove entries explicitly when acted on or deemed irrelevant.

**Flow Example:**
```
Initiative Thread (tick 3) -> writes initiative:explore_fs to working memory
Orient (tick 4)            -> compiles working memory including initiative:explore_fs
Decide (tick 4)            -> sees nudge, decides to act, queues fs.read action
Amend (tick 4)             -> Decide removes initiative:explore_fs (acted on)
Initiative Thread (tick 6) -> sees exploration happened, writes initiative:mission_subgoal
```

## Introspection Query Clarity Fix

The Decide prompt currently lists "Introspection Tools" alongside "Available tools." The LLM conflates the two invocation mechanisms, attempting to call introspection queries like `memory_search` as regular tool actions (`{"tool_name": "memory_search"}`).

**Fix:** Rework the "Introspection Tools" section of `decide-system.md` to make the invocation mechanism visually and semantically distinct:
- Rename the section to emphasize these are internal queries, not external tools
- Add an explicit "these are NOT tool actions" callout
- Show a clear contrast between tool invocation format and query format
- Move the introspection section further from the tools section to reduce visual conflation

## Files Touched

| File | Change |
|------|--------|
| `prompts/decide-system.md` | Collaborator Stance paragraph; introspection query clarity rewrite |
| `prompts/charters/self-critique.md` | Progress/engagement rewrite |
| `prompts/charters/threat-monitor.md` | Curiosity carve-out |
| `prompts/charters/creative-synthesis.md` | New charter file (externalized from Rust) with experiment suggestions |
| `prompts/charters/initiative.md` | New charter file |
| `crates/exoskeleton-threads/src/builtin/mod.rs` | Register Initiative Thread |
| `crates/exoskeleton-threads/src/builtin/initiative.rs` | New thread definition |
| Tests across `exoskeleton-threads` and `exoskeleton-host` | |

## What's Out of Scope

- Bootstrap improvements (budgets, personality, context window, mission override UI)
- Observatory UI changes for initiative visibility (existing views are sufficient)
- Model-specific tuning
- Additional proactive threads beyond Initiative (can add later if one voice isn't enough)

## Testing & Success Criteria

### Unit Tests

- Initiative Thread produces valid output given various vessel states (early tick, mid-lifecycle, stale plan)
- Orientation nudges produced for low tick number / no plan context
- Mission-advancing nudges produced for active mission context
- Reflective nudges produced for stalled plan context
- Output follows `initiative:` key convention
- Self-Critique does not flag exploration as thrashing
- Self-Critique recognizes engagement without formal plan
- Threat Monitor does not flag normal tool usage as overreach
- Creative Synthesis output includes suggested experiments

### Integration Tests

- Idle tick with Initiative Thread nudge produces non-empty actions from Decide step
- Working memory contains `initiative:` entries after Initiative Thread runs
- Initiative entries expire via TTL
- Decide step can reference and act on initiative entries

### Observability

No new Observatory UI needed. Existing infrastructure provides visibility:
- Timeline: actions per tick (idle ticks should show self-directed actions)
- State snapshot: working memory entries (visible `initiative:` nudges)
- Tick detail: thread outputs (Initiative Thread recommendations)
- Event feed: action outcomes (self-directed action success/failure)
