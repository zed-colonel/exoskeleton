# Benchmark Headless Runner Design

> **Pre-Epoch 11 Sprint** — Connect the benchmark harness to the full Vessel machinery
> to produce an initial coding capability baseline.

**Goal:** Enable `exo bench` to boot a real Vessel per task, run the agent against
coding tasks via the inner loop, capture comprehensive metrics, and produce a
baseline measurement that informs whether Epoch 11 is the right next investment.

**Architecture:** Full Vessel boot (Approach A) via `Vessel::start_with_registry_and_backends()`.
One vessel per task for clean isolation. Task prompt injected through the file inbox.
Ticks run via the existing master loop on the Cognitive AQ. Metrics extracted from
`VesselInspector`, `TickRecord`, and artifact store. Sequential task execution;
parallel deferred to E11.

**Tech Stack:** Rust (exoskeleton-host benchmark module), existing Vessel/Kernel/WI Host
infrastructure, serde_json for result serialization, tempfile for workspace isolation.

---

## 1. Headless Runner

### 1.1 Location

The `HeadlessRunner` struct lives in `crates/exoskeleton-host/src/benchmark.rs`,
expanding the existing module that already contains `TaskSpec`, `TaskResult`,
`SuiteResult`, `prepare_workspace()`, `run_verification()`, and reporting functions.

The CLI crate's `commands/bench.rs` stays thin — it parses arguments and delegates
to `HeadlessRunner`.

### 1.2 Lifecycle

```
exo bench --task easy-01.toml --config bench-vessel.toml
    |
    v
HeadlessRunner::run_task(spec, base_config)
    |
    |  1. prepare_workspace()
    |     Copy repo to tempdir. If base_commit set, git checkout.
    |
    |  2. build_vessel_config()
    |     Merge base config + task overrides.
    |     Force: data_dir=tempdir, workspace_root=workspace,
    |            inner_loop.enabled=true, tool_policy.default="allow"
    |
    |  3. Vessel::start_with_registry_and_backends()
    |     Full boot: 10 SQLite stores, Cognitive AQ, WI Host, LLM backend.
    |
    |  4. Submit task prompt to file inbox as MessageEnvelope
    |
    |  5. Poll for completion (see Section 5)
    |     Outer timeout: task.timeout_secs
    |     Inner checks: AgentComplete, max_ticks, budget exhaustion
    |
    |  6. Extract metrics from VesselInspector + TickStore + ArtifactStore
    |
    |  7. Vessel::shutdown()
    |
    |  8. Apply test_patch (if present in task spec)
    |
    |  9. run_verification() in workspace
    |
    | 10. Assemble and return TaskResult
```

### 1.3 Vessel Configuration for Benchmarks

The user provides a base `vessel.toml` via `--config`. The harness merges
task-specific values on top.

**Forced overrides (harness always sets, regardless of config):**

| Field | Value | Reason |
|-------|-------|--------|
| `data_dir` | task tempdir | Isolation |
| `inner_loop.workspace_root` | prepared workspace path | Agent sees the task repo |
| `inner_loop.enabled` | `true` | Benchmarks need the coding loop |
| `tool_policy.default` | `"allow"` | No interactive approval in benchmarks |
| `tool_policy.rules` | `{}` (cleared) | Suppresses per-tool `ask` rules |

Everything else comes from the user's config: LLM backend, model, token limits,
thread enables, tick intervals. Switching models is just editing the vessel.toml.

**Recommended benchmark vessel.toml:**

```toml
[vessel]
mission = "Benchmark runner - resolve coding tasks accurately and efficiently"

[llm.frontier]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"

[llm]
default_backend = "frontier"
max_output_tokens = 4096
timeout_secs = 120

[inner_loop]
enabled = true
max_steps_per_tick = 25
max_tokens_per_session = 500000
timeout_secs = 300
context_window_size = 3
doom_loop_threshold = 3

[cognitive]
tick_interval_ms = 50
master_loop_interval_secs = 1

[tool_policy]
default = "allow"

[threads]
threat_monitor_enabled = false
meta_cognition_enabled = false
creative_synthesis_enabled = false
initiative_enabled = false
self_critique_enabled = true
memory_consolidation_enabled = true
```

### 1.4 Inbox Injection

The task prompt is delivered as a `MessageEnvelope` written to the vessel's file
inbox directory. This is the same path a real `exo code` session uses.

The envelope contains:
- `from`: a synthetic principal (e.g., `benchmark-harness`)
- `content`: the task prompt from the TOML spec
- `timestamp`: current UTC time

The next Perceive phase picks it up, groups it into a Conversation, and feeds it
to Orient/Decide. The agent sees it as a natural user request.

---

## 2. Extended Task Spec Format

The TOML task spec adds optional fields for richer metadata and external benchmark
compatibility. All new fields are optional; existing specs work unchanged.

```toml
[task]
name = "easy-01-add-test"                    # required
repo_path = "../repos/sample-rust"           # required, relative to spec file
prompt = """..."""                            # required
timeout_secs = 300                           # required
max_steps = 25                               # required

# Optional fields
max_ticks = 1                                # omit for unbounded
difficulty = "easy"                          # easy | medium | hard | expert
language = "rust"                            # primary language
tags = ["unit-test", "add-feature"]          # freeform categorization

# External benchmark provenance
source_benchmark = ""                        # e.g. "swe-bench-verified"
source_id = ""                               # original task ID
base_commit = ""                             # git checkout before starting
gold_patch = ""                              # path to reference solution (not shown to agent)

[verify]
command = "cargo test"                       # required
expected_exit_code = 0                       # required
test_patch = ""                              # applied after agent, before verify

# Optional per-task vessel config overrides
[vessel_overrides]
# Keys here merge into the base vessel config
# e.g. max_steps_per_tick = 50
```

**Field semantics:**

- **`max_ticks`**: If omitted, the harness runs until the agent self-declares done
  or `timeout_secs` fires. If set, acts as a ceiling on ticks executed.
- **`base_commit`**: If present, the harness runs `git checkout <sha>` in the
  prepared workspace before booting the vessel.
- **`test_patch`**: If present, applied to the workspace after the agent finishes
  but before running the verify command. This is the SWE-bench evaluation pattern.
- **`gold_patch`**: Never shown to the agent. Stored in results JSON for post-hoc
  analysis (e.g., patch similarity scoring).
- **`vessel_overrides`**: Shallow-merged into the base vessel config for this task
  only. Top-level keys in `[vessel_overrides]` replace the corresponding keys in
  the base config. Nested tables (e.g., `[vessel_overrides.inner_loop]`) replace
  the entire sub-table, not individual fields within it.

---

## 3. Metrics & Result Format

### 3.1 TaskResult

Three tiers: core metrics (always present), execution details (from TickRecords),
and trace data (from artifacts).

```rust
pub struct TaskResult {
    // === Core ===
    pub task_name: String,
    pub passed: bool,
    pub verification_exit_code: i32,
    pub completion_reason: String,
    pub wall_time_secs: f64,
    pub steps_taken: u32,
    pub ticks_used: u32,
    pub tokens: TokenMetrics,
    pub model_used: String,
    pub timestamp: String,

    // === Execution Details ===
    pub tool_calls: HashMap<String, ToolCallStats>,
    pub files_modified: Vec<String>,
    pub lines_added: u32,
    pub lines_removed: u32,
    pub doom_loop_corrections: u32,
    pub llm_cost_cents: f64,
    pub llm_calls: u32,

    // === Trace Data ===
    pub step_trace: Vec<StepTrace>,
    pub context_utilization: ContextUtilization,
    pub tick_details: Vec<TickMetrics>,

    // === Task Metadata ===
    pub difficulty: Option<String>,
    pub language: Option<String>,
    pub tags: Vec<String>,
    pub source_benchmark: Option<String>,
    pub source_id: Option<String>,
}
```

### 3.2 Token Metrics

```rust
pub struct TokenMetrics {
    pub total_in: u64,
    pub total_out: u64,
    pub total: u64,
    pub by_phase: HashMap<String, PhaseTokens>,
}

pub struct PhaseTokens {
    pub tokens_in: u64,
    pub tokens_out: u64,
}
```

Phases: `"decide"`, `"decide_lite"`, `"reflect"`, `"thread"`.

### 3.3 Step Trace

```rust
pub struct StepTrace {
    pub step: u32,
    pub tick: u32,
    pub tool_name: String,
    pub tool_params: serde_json::Value,
    pub outcome: String,
    pub output_summary: String,
    pub reasoning: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub latency_ms: u64,
}
```

- `output_summary` is truncated to 500 characters to keep result files manageable.
- `reasoning` comes from the DecideLite decision artifact's `rationale` field.

### 3.4 Context Utilization

```rust
pub struct ContextUtilization {
    pub total_budget_tokens: u64,
    pub total_used_tokens: u64,
    pub utilization_pct: f64,
    pub sections: HashMap<String, SectionUtilization>,
}

pub struct SectionUtilization {
    pub budget_tokens: u64,
    pub used_tokens: u64,
    pub truncated: bool,
}
```

Captured from a new `ArtifactKind::ContextBreakdown` persisted during Orient.
The Context Compiler already computes this data; it just needs to be stored.

### 3.5 Tick Metrics

```rust
pub struct TickMetrics {
    pub tick_number: u64,
    pub duration_secs: f64,
    pub inner_loop_steps: u32,
    pub llm_calls: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub actions_taken: u32,
    pub actions_succeeded: u32,
    pub completion_reason: String,
}
```

Assembled from individual `TickRecord` entries via `TickStore`.

### 3.6 Metric Sources

| Metric | Source |
|--------|--------|
| pass/fail, wall time | Harness timing + verification command |
| steps, ticks | `TickRecord` count and inner loop step totals |
| tokens (by phase) | `LlmCallRecord` on each `TickRecord` |
| tool calls | `TickRecord.actions_taken` (ActionRecord per action) |
| files, lines | Code diff artifacts (`ArtifactKind::CodeDiff`) |
| doom-loop corrections | Inner loop event count |
| step trace | Decision artifacts + ActionRecord + LlmCallRecord |
| context utilization | New `ArtifactKind::ContextBreakdown` from Orient |
| cost | `LlmCallRecord.cost_cents` aggregated |

### 3.7 Result Serialization

Results serialize to JSON via `serde_json::to_string_pretty()` and are stored
at `benchmarks/results/{label}.json`. The `format_comparison()` function uses
core fields for regression detection (pass_rate, median_steps, median_tokens,
median_time). The expanded structure is backward-compatible.

---

## 4. CLI Interface

### 4.1 Command Signature

```
exo bench --task <file.toml>          # Run single task
exo bench --suite <dir/>              # Run all TOML in directory
exo bench --config <vessel.toml>      # Vessel config (required for live runs)
exo bench --record <label>            # Save results as JSON
exo bench --compare <label>           # Compare against baseline
exo bench --results-dir <path>        # Override results directory
exo bench --dry-run                   # Harness-only (no agent, no LLM)
exo bench --verbose                   # Stream per-step output
```

### 4.2 Behavior Modes

- **Without `--config`**: Implicitly `--dry-run`. Validates task specs, workspace
  isolation, and verification commands without booting a vessel or spending LLM tokens.
  This is the current E10-S3 behavior.
- **With `--config`**: Live mode. Boots a vessel per task with the specified LLM
  backend and runs the agent.
- **`--dry-run` with `--config`**: Validates config loading and workspace preparation
  but skips vessel boot.

### 4.3 Output Format

**Normal mode:**

```
====================================================
  Exoskeleton Benchmark Report
  Config: bench-vessel.toml (claude-sonnet-4-20250514)
  Date: 2026-04-06T14:30:00Z
====================================================

  easy-01-add-test .............. PASS   12s   8 steps   3,240 tok   $0.02
  easy-02-fix-typo .............. PASS    6s   4 steps   1,820 tok   $0.01
  easy-03-add-doc-comment ....... PASS    4s   3 steps   1,210 tok   $0.01

----------------------------------------------------
  Pass rate:     3/3 (100%)
  Median time:   6.0s
  Median steps:  4
  Median tokens: 1,820
  Total cost:    $0.04
====================================================
```

**Verbose mode** adds per-step lines:

```
  easy-01-add-test
    [1] code.read src/lib.rs -> 45 lines (Success)
    [2] code.edit src/lib.rs -> +12 -0 lines (Success)
    [3] shell.exec "cargo test" -> exit 0 (Success)
    ... PASS   12s   8 steps
```

**Comparison mode** (`--compare`) shows deltas:

```
  Comparing against baseline: v0.1
  easy-01-add-test   PASS -> PASS   12s -> 10s   8 -> 6 steps
  easy-02-fix-typo   PASS -> PASS    6s ->  5s   4 -> 4 steps
  Pass rate: 100% -> 100% (no change)
  Median steps: 4 -> 4 (no change)
```

---

## 5. Completion Detection

### 5.1 State Machine

The harness wraps the entire poll loop in `tokio::time::timeout(task.timeout_secs)`.
Within that, it polls `TickStore` after each tick and applies rules in priority order:

1. **Task timeout expired** -> Stop. Record `completion_reason: "Timeout"`.
2. **Agent self-declared done** -> Inner loop returned `AgentComplete` (empty actions).
   Record `completion_reason: "AgentComplete"`. This is the happy path.
3. **Max ticks reached** -> `max_ticks` is set and hit. Record
   `completion_reason: "MaxTicks"`.
4. **Inner loop budget exhaustion within a tick** -> `StepLimit`, `TokenLimit`,
   `TimeLimit`, or `DoomLoop`. If more ticks are allowed (no `max_ticks` or below
   ceiling), continue to next tick. The agent gets a fresh inner loop session with
   Reflect's feedback. If at the ceiling, stop and record the specific reason.
5. **Unbounded mode, inner loop exhausted** -> Continue to next tick. Only the task
   timeout (rule 1) stops this.

### 5.2 Multi-Tick Continuation

When the inner loop exhausts its budget within a tick but more ticks are available,
the next tick runs the full PODAARA cycle:

- **Reflect** evaluates what happened (code changes, failures, progress)
- **Amend** persists updated state and working memory
- **Perceive** (next tick) picks up the updated snapshot
- **Orient** compiles fresh context with Reflect's observations
- **Decide** gets a "second chance" with self-critique feedback

This is where Exoskeleton's multi-tick architecture adds value over flat-loop agents.
The agent effectively gets a reset with an internal review.

### 5.3 Polling Implementation

```rust
loop {
    // Wait for next tick to complete
    let ticks = wait_for_ticks(&storage, expected_tick, poll_timeout).await;

    // Inspect latest tick
    let latest = inspector.tick_detail(tick_id)?;
    let reason = extract_completion_reason(&latest);

    match reason {
        AgentComplete => break,
        StepLimit | TokenLimit | TimeLimit | DoomLoop => {
            ticks_used += 1;
            if max_ticks_reached(ticks_used, spec.max_ticks) {
                break;
            }
            // else: continue, next tick will fire
        }
        _ => {
            ticks_used += 1;
            // continue polling
        }
    }
}
```

---

## 6. Per-Step Trace & Artifact Capture

### 6.1 Step Trace Assembly

After vessel shutdown, the harness reads from the artifact store and event ledger
to assemble `StepTrace` entries:

- **Decision artifacts** (`ArtifactKind::Decision`): Contains `reasoning`,
  `tool_name`, `params`, `rationale` for each DecideLite call.
- **LlmCallRecord** (on `TickRecord`): Contains `tokens_in`, `tokens_out`,
  `latency_ms`, `model`, `cost_cents`.
- **ActionRecord** (on `TickRecord`): Contains `action_type`, `target`, `outcome`,
  `receipt_ref`.
- **Receipt artifacts**: The tool output, referenced by `receipt_ref`. Truncated
  to 500 chars for `output_summary`.
- **CodeDiff artifacts** (`ArtifactKind::CodeDiff`): Unified diff, lines_added,
  lines_removed for mutating code tools.

### 6.2 Context Utilization

The Context Compiler already computes per-section token counts during `compile()`.
This design adds:

- A `ContextBreakdown` struct on `CompiledContext` containing per-section
  `{budget_tokens, used_tokens, truncated}`.
- The Orient phase stores this as `ArtifactKind::ContextBreakdown`.
- The harness reads it post-run to populate `ContextUtilization`.

This is a small addition to Orient — the data is already computed, it just needs
to be persisted alongside the compiled context.

### 6.3 Verbose Streaming

When `--verbose` is active, the harness subscribes to the vessel's `event_tx`
broadcast channel and prints `InnerLoopStep` events as they arrive:

```
  [step] code.read src/lib.rs -> 45 lines (Success)
```

This provides real-time visibility without polling. The subscription is dropped
on task completion.

---

## 7. Scope & Boundaries

### 7.1 In Scope (This Sprint)

- `HeadlessRunner` struct with `run_task()` and `run_suite()`
- Extended `TaskSpec` with all optional fields
- Expanded `TaskResult` with full metrics, trace, and context utilization
- `ArtifactKind::ContextBreakdown` persisted during Orient
- CLI updates: `--config`, `--dry-run`, `--verbose`
- Updated `format_report()` and `format_comparison()` for expanded metrics
- `base_commit` checkout in workspace preparation
- `test_patch` application before verification
- Completion detection state machine
- Verbose streaming via event broadcast subscription
- Run initial baseline against the 3 easy tasks

### 7.2 Out of Scope (Deferred)

- Parallel task execution (`--parallelism` flag) -> E11
- SWE-bench adapter/converter -> post-baseline
- TerminalBench integration -> post-baseline
- Gold patch similarity scoring -> post-baseline
- CI/CD integration (JUnit, GitLab report formats) -> future
- Dashboard / web UI for results -> Observatory integration, future
- Code Review thread -> E11 or post-E11
- Repository map / symbol extraction -> E11 investigation

### 7.3 Non-Goals

- Optimizing agent performance (prompt tuning, model selection) -> done after
  baseline exists
- Changing the inner loop, Decide, or Reflect behavior -> this sprint only
  observes, doesn't modify
- Multi-language support -> task specs are language-agnostic already; adding
  non-Rust sample repos is a content task, not an infrastructure task
