# Acceptance Test Taxonomy

Maps each acceptance criterion and sacred invariant to the tests that verify it.

All acceptance tests live in `tests/acceptance/tests/`. They boot a full Vessel with mock LLM backends and exercise the complete stack.

---

## Acceptance Criteria

### Criterion A: Thread Convergence

> Three built-in threads converge into one StateSnapshot, all running on the Cognitive AQ.

| Test | File | Key Assertions |
|------|------|----------------|
| `thread_convergence_3_threads_10_ticks` | `thread_convergence.rs` | Final snapshot contains thread_summaries for all 3 threads (Threat Monitor, Self-Critique, Memory Consolidation) |
| `tick_records_have_thread_contributions` | `thread_convergence.rs` | Every tick has non-empty thread_contributions |
| `thread_summaries_updated_in_snapshot` | `thread_convergence.rs` | Snapshot has >= 2 thread summaries with non-empty names |
| `thread_output_artifacts_exist_and_parse` | `thread_convergence.rs` | ThreadOutput artifacts stored in artifact store (I3), valid JSON with summary field |
| `memory_consolidation_runs_within_10_ticks` | `thread_convergence.rs` | Memory Consolidation (EveryNTicks(5)) triggers at least once within 10 ticks |
| `no_thread_work_on_tool_aq` | `thread_convergence.rs` | Thread contributions exist on Cognitive AQ; no ThreadOutput artifacts appear as Receipt (tool-side) |

### Criterion B: Kill/Restart Durability

> Relationship and tick state survives kill/restart.

| Test | File | Key Assertions |
|------|------|----------------|
| `relationship_survives_kill_restart` | `relationship_durability.rs` | Boot, submit messages, run 5 ticks, drop (crash), restart, run 2 more ticks, tick_number >= 7 |
| `tick_numbers_monotonic_across_restart` | `relationship_durability.rs` | Tick numbers strictly increasing across crash boundary |
| `all_stores_accessible_after_restart` | `relationship_durability.rs` | All 8 StorageManager stores open and readable after crash |
| `both_engines_recover_independently` | `relationship_durability.rs` | cognitive-aq/ and wi/ directories exist after crash; restart succeeds (I9) |
| `trust_levels_accurate_after_restart` | `relationship_durability.rs` | Relationship store accessible and coherent after restart |

### Criterion C: Thread Replay

> Thread outputs are durable artifacts replayable from the artifact chain.

| Test | File | Key Assertions |
|------|------|----------------|
| `thread_replay_artifact_chain` | `thread_replay.rs` | ThreadOutput artifacts exist after 5 ticks; each is valid JSON with summary field |
| `thread_output_artifacts_are_valid` | `thread_replay.rs` | Content type is application/json, kind is ThreadOutput, content non-empty |
| `thread_output_matches_contribution` | `thread_replay.rs` | Every tick has contributions with non-empty summaries |
| `llm_response_artifacts_exist` | `thread_replay.rs` | LlmResponse artifacts exist after 3 ticks; each tick has LLM call records |
| `tick_records_form_continuous_chain` | `thread_replay.rs` | Tick numbers 1..5 continuous; each tick has snapshot_before artifact |

### Criterion D: Atomic Align + Crash

> Align step writes atomically; crash preserves ledger coherence.

| Test | File | Key Assertions |
|------|------|----------------|
| `align_writes_survive_crash` | `atomic_align.rs` | Relationship ledger entry count matches pre-crash vs. post-crash |
| `no_duplicate_ledger_entries` | `atomic_align.rs` | No duplicate ledger entry IDs across crash/restart |
| `ledger_entries_chronological` | `atomic_align.rs` | Ledger entries in chronological order |
| `sqlite_wal_mode_active` | `atomic_align.rs` | relationships.db uses WAL journal mode |

### Criterion E: Budget Enforcement

> Cognitive and tool budgets configured, tracked, and enforced independently at runtime.

**Config validation tests:**

| Test | File | Key Assertions |
|------|------|----------------|
| `budget_status_in_snapshot_defaults_to_unlimited` | `budget_enforcement.rs` | Without budget config, BudgetStatus shows tokens remaining > 0 (unlimited defaults) |
| `budget_config_validation_works` | `budget_enforcement.rs` | Valid config passes; zero time_window_secs and zero max_invocations fail validation |
| `cognitive_and_tool_budgets_independent_types` | `budget_enforcement.rs` | CognitiveBudgetConfig and ToolBudgetConfig are structurally independent; validate independently (I6, I9) |
| `vessel_ticks_without_budget_config` | `budget_enforcement.rs` | 5 ticks complete without budget config; budget_tracker and tool_budget_gate are None |
| `budget_status_snapshot_reflects_no_budget` | `budget_enforcement.rs` | BudgetStatus.is_exhausted() is false; total_tokens_remaining() > 0 without config |
| `escalation_policy_defaults_reasonable` | `budget_enforcement.rs` | Default escalation threshold > 0; default frontier call limit > 0 |

**Runtime enforcement tests:**

| Test | File | Key Assertions |
|------|------|----------------|
| `budget_enabled_vessel_boots_ticks_and_shuts_down` | `budget_enforcement.rs` | Budget-enabled Vessel boots, runs 5 ticks, shuts down within 30s (no deadlock); budget_tracker and tool_budget_gate are Some |
| `cognitive_budget_tracks_consumption` | `budget_enforcement.rs` | After 5 ticks, remaining_local_tokens < initial budget; BudgetStatus in snapshot reflects consumption |
| `cognitive_budget_exhaustion_reflected_in_snapshot` | `budget_enforcement.rs` | Small budget (200 tokens) exhausted after ~4 ticks; check_cognitive_budget() returns Exhausted; BudgetStatus.is_exhausted() true |
| `tool_budget_gate_initialized_and_enforces` | `budget_enforcement.rs` | Gate initialized from config; allows invocations up to limit; blocks after max_invocations; reset_window() replenishes |
| `budget_window_timer_resets_counters` | `budget_enforcement.rs` | 2-second window timer fires and replenishes tokens after consumption; post-reset remaining > pre-reset remaining |

### Criterion F: Inspection Surface

> `exo inspect` and daemon endpoints show full vessel state.

| Test | File | Key Assertions |
|------|------|----------------|
| `status_endpoint_shows_full_state` | `inspection_surface.rs` | GET /api/v1/status returns mission, tick_number >= 5, >= 2 thread summaries |
| `ticks_endpoint_returns_history` | `inspection_surface.rs` | GET /api/v1/ticks?limit=5 returns 5 ticks with thread_contributions |
| `threads_endpoint_lists_all_three` | `inspection_surface.rs` | GET /api/v1/threads returns 3 threads: Threat Monitor, Self-Critique, Memory Consolidation |
| `events_endpoint_returns_events` | `inspection_surface.rs` | GET /api/v1/events returns non-empty event list |
| `engines_endpoint_shows_both_engines` | `inspection_surface.rs` | GET /api/v1/engines returns cognitive and tool objects |
| `metrics_endpoint_has_live_data` | `inspection_surface.rs` | GET /metrics contains exo_ticks_total |
| `healthz_returns_200` | `inspection_surface.rs` | GET /healthz returns 200 |
| `ready_reflects_engine_state` | `inspection_surface.rs` | GET /ready returns 200 or 503 |
| `websocket_liveness_broadcast_events` | `inspection_surface.rs` | WebSocket at /api/v1/ws receives LiveEvent messages during ticks |
| `d2_api_completeness` | `inspection_surface.rs` | All D2 endpoints (artifacts, memory, snapshots, inbox-history, config) return valid responses |
| `d2_config_sanitization` | `inspection_surface.rs` | GET /api/v1/config returns SanitizedConfig (env var names, not key values) (I4) |

### Criterion G: Engine Isolation Proof (I9)

> Cognitive work is unaffected by Tool AQ saturation.

| Test | File | Key Assertions |
|------|------|----------------|
| `cognitive_ticks_unaffected_by_tool_saturation` | `engine_isolation.rs` | 50 delay flows on Tool AQ; all 10 cognitive ticks complete |
| `all_ticks_complete_under_tool_load` | `engine_isolation.rs` | 20 tool flows submitted; 5 cognitive ticks complete with completed_at timestamps |
| `tool_flows_eventually_complete` | `engine_isolation.rs` | Delay flows submitted to Tool AQ complete successfully |
| `thread_contributions_not_starved` | `engine_isolation.rs` | 30 tool flows; every tick still has thread contributions (I9) |
| `no_cross_engine_task_contamination` | `engine_isolation.rs` | Separate engine directories; ThreadOutput artifacts on cognitive side; no cross-engine error events |

### Smoke Test

| Test | File | Key Assertions |
|------|------|----------------|
| `vessel_boots_ticks_and_shuts_down` | `smoke.rs` | Vessel boots, completes >= 1 tick, shuts down cleanly |

---

## Invariant Coverage

Maps each sacred invariant to the acceptance tests that verify it.

| Invariant | Description | Verified By |
|-----------|-------------|-------------|
| **I1** | No external state changes outside adapters | `no_thread_work_on_tool_aq` (threads don't produce tool-side artifacts) |
| **I2** | Every external action gets `run_id` | ActionQueue run lifecycle (unit tests in AQ crate) |
| **I3** | Everything replayable from both AQ WALs + artifacts | `thread_output_artifacts_exist_and_parse`, `thread_replay_artifact_chain`, `tick_records_form_continuous_chain`, `llm_response_artifacts_exist` |
| **I4** | Least privilege by default | API key handling (unit tests in config.rs); frontier config stores env var name, not key |
| **I5** | Context compiled, not accumulated | Context Compiler unit tests (exoskeleton-memory); thread context compilation |
| **I6** | Budgets enforced independently per engine | `cognitive_and_tool_budgets_independent_types`, `vessel_ticks_without_budget_config`, `cognitive_budget_tracks_consumption`, `cognitive_budget_exhaustion_reflected_in_snapshot`, `tool_budget_gate_initialized_and_enforces` |
| **I7** | Single coherent workspace | `thread_convergence_3_threads_10_ticks` (3 threads converge into 1 snapshot) |
| **I8** | Relationship awareness is durable and explicit | `relationship_survives_kill_restart`, `align_writes_survive_crash`, `trust_levels_accurate_after_restart` |
| **I9** | Cognitive and tool execution isolated | `cognitive_ticks_unaffected_by_tool_saturation`, `both_engines_recover_independently`, `no_cross_engine_task_contamination`, `no_thread_work_on_tool_aq`, `thread_contributions_not_starved` |

---

## Test Execution

```bash
# Run all acceptance tests
cargo test -p exoskeleton-acceptance --test '*'

# Run a specific criterion
cargo test -p exoskeleton-acceptance --test thread_convergence
cargo test -p exoskeleton-acceptance --test relationship_durability
cargo test -p exoskeleton-acceptance --test thread_replay
cargo test -p exoskeleton-acceptance --test atomic_align
cargo test -p exoskeleton-acceptance --test budget_enforcement
cargo test -p exoskeleton-acceptance --test inspection_surface
cargo test -p exoskeleton-acceptance --test engine_isolation
cargo test -p exoskeleton-acceptance --test smoke

# Run all workspace tests (unit + integration + acceptance)
cargo test --workspace
```

Acceptance tests use mock LLM backends (`MockLlmBackend`) and the full connector registry (Delay, FsRead, FsWrite, HttpRequest). Tests use fast tick intervals (10ms) and 30-second lease timeouts for CI stability.
