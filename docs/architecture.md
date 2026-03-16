# Architecture Reference

## Overview

Exoskeleton operates two physically separate ActionQueue engines. This is **sacred invariant I9** -- cognitive and tool execution must never merge into a single engine. The separation provides:

- **Isolation** -- a saturated Tool AQ cannot starve cognitive processing
- **Independent scheduling** -- each engine has its own tick interval, concurrency, and budget
- **Debuggability** -- cognitive and tool work have separate WALs, separate dispatch loops, separate failure domains
- **Replay fidelity** -- the Cognitive AQ WAL captures the agent's reasoning; the Tool AQ WAL captures the agent's effects on the world

---

## Dual-Engine Architecture

```
                            Exoskeleton Vessel
    ┌───────────────────────────────────────────────────────┐
    │                                                       │
    │  ┌─────────────────────────────────────────────────┐  │
    │  │              Cognitive AQ                        │  │
    │  │  (owned by exoskeleton-host)                    │  │
    │  │                                                 │  │
    │  │  WAL: {data_dir}/cognitive-aq/                  │  │
    │  │                                                 │  │
    │  │  ┌─────────────────────────────────────────┐    │  │
    │  │  │  Master Loop (PODAARA)                  │    │  │
    │  │  │  Thread Executions                      │    │  │
    │  │  │  LLM Inference (decide + threads)       │    │  │
    │  │  └─────────────────────────────────────────┘    │  │
    │  └─────────────────────────────────────────────────┘  │
    │                         │                             │
    │                    Act step                           │
    │                  (sole boundary)                      │
    │                         │                             │
    │  ┌─────────────────────────────────────────────────┐  │
    │  │              Tool AQ                            │  │
    │  │  (owned by WorldInterface Host)                 │  │
    │  │                                                 │  │
    │  │  WAL: {data_dir}/wi/aq/                         │  │
    │  │                                                 │  │
    │  │  ┌─────────────────────────────────────────┐    │  │
    │  │  │  Adapter Invocations (delay, fs, http)  │    │  │
    │  │  │  Workflow Orchestration                  │    │  │
    │  │  │  External I/O                           │    │  │
    │  │  └─────────────────────────────────────────┘    │  │
    │  └─────────────────────────────────────────────────┘  │
    │                                                       │
    │  ┌─────────────────────────────────────────────────┐  │
    │  │  Persistence Layer (8 SQLite stores)            │  │
    │  │  {data_dir}/exo/                                │  │
    │  └─────────────────────────────────────────────────┘  │
    └───────────────────────────────────────────────────────┘
```

---

## Cognitive AQ

The Cognitive AQ is owned directly by `exoskeleton-host`. It runs all cognitive work:

- **Master loop ticks** -- scheduled as a recurring task (`Repeat(10_000, interval_secs)`)
- **Thread executions** -- Threat Monitor, Self-Critique, Memory Consolidation (and custom threads)
- **LLM inference** -- both Decide step and thread-level calls route through the Cognitive AQ's handler

The Cognitive AQ has its own:

- **WAL** at `{data_dir}/cognitive-aq/`
- **Scheduler** with configurable tick interval (default: 100ms)
- **Dispatch loop** with configurable concurrency (default: 4 workers)
- **Lease timeout** (default: 600s) -- must account for worst-case Act step duration
- **Budget pool** (optional) -- Token and CostCents dimensions on the master loop task

LLM calls are cognitive work (I1). They are dispatched through the Cognitive AQ handler, not through WI adapters. The Decide step and each thread call the LLM backend directly (avoiding the LlmClient AQ path to prevent deadlock -- see H-1 pattern).

---

## Tool AQ

The Tool AQ is owned by the WorldInterface Host. Exoskeleton never accesses it directly -- the WI Host is a black box that accepts invocation requests and returns results.

The Tool AQ runs:

- **Adapter invocations** -- `delay`, `fs.read`, `fs.write`, `http.request`
- **Workflow orchestration** -- multi-step FlowSpec DAGs
- **External I/O** -- all state changes to the outside world

The Tool AQ has its own:

- **WAL** at `{data_dir}/wi/aq/`
- **Context store** at `{data_dir}/wi/context.db`
- **Scheduler** with configurable tick interval (default: 50ms)
- **Dispatch loop** with configurable concurrency (default: 4 workers)

The Tool AQ's budget is enforced by the `ToolBudgetGate` at the Act step boundary, independently of the Cognitive AQ's budget (I6).

---

## PODAARA Cycle

Each master loop tick executes one complete PODAARA cycle:

```
  ┌──────────────────────────────────────────────────────────────┐
  │                                                              │
  │  ┌───────────┐    ┌────────┐    ┌────────┐    ┌───────┐    │
  │  │ Perceive  │───>│ Orient │───>│ Decide │───>│ Align │    │
  │  │           │    │        │    │  (LLM) │    │(trust)│    │
  │  └───────────┘    └────────┘    └────────┘    └───┬───┘    │
  │        ^                                          │        │
  │        │                                          v        │
  │  ┌───────────┐    ┌──────────┐              ┌─────────┐    │
  │  │  Amend    │<───│ Reflect  │<─────────────│   Act   │    │
  │  │(persist)  │    │          │              │(tools)  │    │
  │  └───────────┘    └──────────┘              └─────────┘    │
  │                                                  │         │
  └──────────────────────────────────────────────────┼─────────┘
                                                     │
                                          ┌──────────v──────────┐
                                          │  Tool AQ (WI Host)  │
                                          │  Sole boundary (I9) │
                                          └─────────────────────┘
```

**Step details:**

| Step | Input | Output | Engine |
|------|-------|--------|--------|
| **Perceive** | Inbox, previous tick | Envelopes, signals, thread outputs | Cognitive |
| **Orient** | Snapshot, perception, memory, relationships | Compiled context window | Cognitive |
| **Decide** | Oriented context | Action plan (LLM inference) | Cognitive |
| **Align** | Decision, relationship snapshot | Approved/blocked actions | Cognitive |
| **Act** | Approved actions | Execution results | **Tool** (boundary crossing) |
| **Reflect** | Act results | Outcome assessment | Cognitive |
| **Amend** | All step results | Updated snapshot, tick record, events | Cognitive (persist) |

Thread execution occurs between Perceive and Orient. Due threads are executed sequentially, each producing a summary and recommendations that feed into the perception.

---

## Act Step Boundary

The Act step is the sole crossing point from Cognitive AQ to Tool AQ (Charter Section 3.1.3, IBP Section 3).

Rules:

1. Only actions **approved by the Align step** may be executed
2. The WI Host is accessed through a `WiHostSlot` (`Arc<Mutex<Option<EmbeddedHost>>>`)
3. Each action invocation creates an ephemeral FlowSpec and executes through the full Tool AQ pipeline
4. Results are recorded as `ActionExecution` records with `ActionOutcome` (Success, Failure, RateLimited)
5. The ToolBudgetGate checks tool invocation count before each action (I6)
6. The cognitive task remains in Running state while waiting for tool completion

Threads never invoke tools. They produce recommendation artifacts only.

---

## Thread Convergence Model

Multiple cognitive threads execute per tick, but their outputs converge into a single coherent StateSnapshot:

```
  Threat Monitor ──┐
                   │
  Self-Critique ───┼──> ThreadContributions ──> StateSnapshot
                   │         (per tick)          (one per tick)
  Memory Consol. ──┘
```

Thread execution is sequential within the master loop handler (not parallel AQ child tasks). This prevents resource contention and ensures deterministic ordering. Each thread:

1. Receives a compiled thread context (ThreadContextCompiler)
2. Calls the LLM backend directly (H-1 pattern)
3. Returns a `ThreadResponse` with summary and recommendations
4. Its output is stored as a `ThreadOutput` artifact (I3)

The Amend step populates `snapshot.thread_summaries` from the thread registry, ensuring all thread contributions are visible in the next tick's Orient phase.

---

## Relationship Substrate

The relationship substrate provides trust-based governance:

```
  Perceive                Align                    Amend
     │                      │                        │
     │  process signals     │  check alignment       │  store snapshot
     │  ────────────>       │  ────────────>         │  ────────────>
     │                      │                        │
     v                      v                        v
  RelationshipLedger   RelationshipSnapshot    ArtifactStore
  (append-only)        (compiled per-principal) (snapshot ref)
```

**Trust computation:** Starts at 0.5 for new principals, then adjusts:

- CommitmentFulfilled: +0.05
- CommitmentBroken: -0.15
- FeedbackReceived: +0.02
- AlignmentMismatch: -0.05
- Clamped to [0.0, 1.0]

**Alignment gating:** The Align step checks trust before approving actions:

- Normal actions require trust >= 0.3 (configurable)
- Destructive actions (e.g., `fs.write`) require trust >= 0.6 (configurable)
- Empty relationship snapshot: passthrough (new principals can act)

**Storage layer.** The persistence layer comprises 8 SQLite stores managed by `StorageManager`. See [Data Directory Layout](data-directory.md) for the full directory structure and store catalog.

---

## Memory & Context Compilation

Exoskeleton maintains three memory tiers, all feeding into a token-budgeted context window each tick:

**Working context** lives directly in the StateSnapshot. It is a short string summarizing the vessel's immediate focus — updated each tick by the Amend step.

**Episodic summaries** are recent tick digests stored in `MemoryStore`. The Orient step fetches the 10 most recent summaries to give the LLM a sense of what just happened.

**Long-term notes** are operator-provided or consolidation-produced durable memories. The Memory Consolidation thread periodically promotes salient observations from episodic summaries into long-term storage.

### Context Compiler

The `ContextCompiler` in `exoskeleton-memory` assembles a token-budgeted prompt from these sources:

```
  ContextSources                    ContextCompiler                 CompiledContext
  ┌──────────────────┐              ┌──────────────┐               ┌────────────────┐
  │ mission          │              │              │               │ prompt (String) │
  │ snapshot         │──────────────│  render      │──────────────>│ total_tokens    │
  │ relationship     │   sections   │  budget      │   truncate    │ budget          │
  │ thread outputs   │──────────────│  truncate    │──────────────>│ sections[]      │
  │ recent events    │              │  assemble    │               │ truncated[]     │
  │ episodic memory  │              │              │               │                 │
  │ long-term notes  │              └──────────────┘               └────────────────┘
  └──────────────────┘
```

Each source is rendered into a named section. The system section (vessel identity, mission) is never truncated. Remaining sections share the budget proportionally and are truncated from the bottom when necessary. This enforces **invariant I5** — context is compiled fresh each tick, never accumulated.

Thread context compilation follows the same pattern but with a per-thread budget. The charter section is never truncated; snapshot and recent outputs share the remaining budget 50/50.

---

## Budget Enforcement

Budget enforcement operates in two layers:

### Layer 1: AQ BudgetGate (hard stop)

The ActionQueue BudgetGate halts task dispatch when budget dimensions are exhausted. This is a coarse-grained stop — once triggered, no more cognitive or tool tasks run until the budget window resets.

### Layer 2: Exoskeleton CognitiveBudgetTracker (fine-grained)

The `CognitiveBudgetTracker` in `exoskeleton-host` provides token-level accounting:

- **Per-backend tracking** — local and frontier model tokens tracked separately
- **Per-thread caps** — no single thread can consume more than `per_thread_token_cap` per tick
- **Per-tick caps** — total token consumption across all threads capped per tick
- **Window-based replenishment** — a timer task resets counters at `time_window_secs` intervals

### Thrash Detection

The `ThrashDetector` analyzes recent TickRecords for pathological patterns:

- **Action repetition** — the same action attempted repeatedly
- **Stagnation** — no meaningful state change across ticks
- **Token waste** — high token consumption with no useful output

These produce a graduated `ThrashLevel` (None → Low → High → Critical) visible in the BudgetStatus.

### Model Escalation

Consecutive LLM failures trigger frontier model escalation in the Decide step. The escalation policy checks:

- Consecutive failure count (configurable, default: 3)
- Remaining frontier budget and call limits
- Stake-based escalation for high-risk action types

### ToolBudgetGate

The `ToolBudgetGate` rate-limits tool invocations at the Act step boundary, enforcing **invariant I6** (budgets enforced independently per engine). It counts invocations per window and blocks execution when the limit is reached, returning `ActionOutcome::RateLimited`.

---

## Prompt System

The prompt system (introduced in Epoch 0) externalizes all LLM prompts into editable Markdown files.

### PromptRegistry

`PromptRegistry` in `exoskeleton-core` is a pure in-memory `HashMap<String, String>`. It provides:

- `resolve(name, vars)` — look up a template by name and substitute `{{variable}}` placeholders
- `register(name, template)` — add or replace a template at runtime
- `with_defaults()` — load compiled-in fallback templates

### Prompt Files

Nine prompt files live in the `prompts/` directory:

| File | Purpose | Variables |
|------|---------|-----------|
| `system-section.md` | Orient step system context | `vessel_id`, `mission` |
| `decide-system.md` | Decide step system prompt | `vessel_id`, `mission` |
| `decide-user.md` | Decide step user prompt | `context` |
| `bootstrap-system.md` | Bootstrap first-contact system | `mission` |
| `charter-threat-monitor.md` | Threat Monitor thread | `thread_id`, `thread_name`, `tick_number` |
| `charter-self-critique.md` | Self-Critique thread | `thread_id`, `thread_name`, `tick_number` |
| `charter-memory-consolidation.md` | Memory Consolidation thread | `thread_id`, `thread_name`, `tick_number` |
| `bootstrap-extract-identity.md` | Identity extraction | *(none)* |
| `bootstrap-verify.md` | Config verification | `config_summary` |

### Three-Tier Loading

Templates are loaded with a priority chain:

1. **Operator override** — `{data_dir}/prompts/{name}.md` (highest priority)
2. **Project default** — `prompts/{name}.md` in the working directory
3. **Compiled-in fallback** — `include_str!()` baked into the binary (lowest priority)

This allows operators to customize prompts without recompilation, while always having a working fallback.

### Charter Hot-Reload

Thread charters can be reloaded at runtime via `POST /api/v1/charters/reload` (CLI: `exo reload-charters`). The `ThreadRegistry::reload_charters()` method re-reads charter templates from disk and updates the `PromptRegistry`, taking effect on the next tick.

See [Configuration Reference](configuration.md) for prompt override configuration.

---

## References

- [Exoskeleton Charter](../exoskeleton_charter_1.0.md) -- Section 3.1.3 (Dual-Engine Architecture)
- [Invariant Boundaries Policy](../exoskeleton_invariant_boundaries_policy_1.0.md) -- Section 3 (Engine Isolation)
- [Scope Appendix](../exoskeleton_scope_appendix_1.0.md) -- Section 5 (Acceptance Criteria)
- [Configuration Reference](configuration.md)
- [Data Directory Layout](data-directory.md)
