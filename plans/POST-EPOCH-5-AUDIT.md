# Post-Epoch 5 Comprehensive Audit

**Date:** 2026-03-27
**Scope:** All four repositories — Exoskeleton, Observatory, WorldInterface, ActionQueue
**Reference:** [Roadmap v2](ROADMAP-v2.md), [Epoch 5 Implementation Plan](EPOCH-5-IMPLEMENTATION-PLAN.md)

---

## Executive Summary

Four repositories, 28+ Rust crates, 1 React SPA, **~2,300+ tests all passing** (with 2 minor exceptions). The codebase demonstrates remarkably consistent engineering discipline across the entire stack. The project has successfully delivered Epochs 0 through 5 against the ROADMAP-v2 plan, and the separation of concerns between all four repos is architecturally clean.

**Overall Grade: A**

The most important finding is that **the roadmap is now the weakest artifact** — the code has outpaced it significantly, particularly in ActionQueue where Epoch 7's "future work" already exists as implemented features.

---

## 1. Roadmap Completeness

### All Epoch 5 Exit Criteria Met

| Criterion | Status | Implementation |
|-----------|--------|----------------|
| Meta-Cognition thread detects inefficiencies, proposes charter changes | **Done** | `meta_cognition.rs`, `CharterProposal`, governance endpoints |
| Agent queries own stores from Decide step | **Done** | `IntrospectionService` (10 query variants), multi-turn Decide loop |
| Watch primitives enable autonomous monitoring | **Done** | `WatchDefinition`, `WatchExecutor`, `WatchStore` (threshold + poll) |
| Tool discovery / hot-loading at runtime | **Done** | `POST /connectors/load`, `DELETE /connectors/:name`, RwLock registry |
| Observatory relationship graph | **Done** | `RelationshipGraph.tsx` (d3-force) |
| Trust trajectory charts | **Done** | `TrustTrajectoryChart.tsx` (recharts) |
| Memory search / filtering | **Done** | `MemorySearchInput.tsx`, debounced input, tag filtering |

All 8 work items (W-17, W-18, W-19, W-20, W-22, W-25, W-26, W-30) implemented across 4 sprints.

### Epoch 7 Scope is Dramatically Reduced

The roadmap says: *"AQ currently runs embedded. Platform AQ needs gRPC or HTTP API, multi-tenant dispatch, task metadata."*

**ActionQueue already has:**
- `actionqueue-daemon` — HTTP API (Axum) with `/api/v1/` introspection + `/api/v2/` actor/platform endpoints
- `actionqueue-actor` — Actor registration, heartbeat monitoring, capability-based routing, department grouping
- `actionqueue-platform` — Multi-tenant isolation, RBAC enforcement (`Operator`, `Auditor`, `Gatekeeper`, `Custom`), append-only ledgers (audit, decision, relationship, incident)
- Control endpoints: cancel, pause/resume engines

**Epoch 7 estimated at 7-9 sprints should be revised to ~3-5 sprints.** The platform foundation already exists.

---

## 2. Separation of Concerns

### Dependency Boundaries — Verified Clean

```
ActionQueue     ← zero deps on Exo/WI/Obs (fully independent, crates.io publishable)
WorldInterface  ← ActionQueue only (zero Exo/Obs deps)
Exoskeleton     ← ActionQueue + WorldInterface
Observatory     ← exoskeleton-core only (pure domain types, no runtime)
```

Verified by searching all `Cargo.toml` files across all repos. Zero violations.

### Per-Repo Assessment

**ActionQueue** — 11 crates, fully independent. `#![forbid(unsafe_code)]` on every crate. Feature-flag architecture (workflow, budget, actor, platform) enables à la carte usage. Can be used by any project, published to crates.io.

**WorldInterface** — 10 crates. Depends on ActionQueue only. Could function as a standalone workflow engine today — FlowSpec compiler, coordinator, 7 native connectors, WASM runtime, webhook triggers all work without Exoskeleton awareness. `EmbeddedHost` is the clean integration surface.

**Observatory** — Depends on `exoskeleton-core` for shared domain types (VesselId, PrincipalId) but communicates with vessels exclusively via HTTP/WebSocket. Docker management abstracted via `DockerManager` trait. CLAUDE.md: *"Observatory Server does NOT depend on exoskeleton-host, ActionQueue, or WorldInterface."* Verified.

**Exoskeleton** — 7 crates with clean DAG: `core → memory/relationship/threads → host → daemon → cli`. Trait-driven architecture with store traits defined in leaf crates, implemented in host. All I/O concentrated in host crate.

### Minor Concern

Observatory's `exoskeleton-core` dependency is the thinnest possible coupling, but if Observatory ever needs to support non-Exoskeleton agents, these types should be extracted into a shared types crate. Acceptable for current scope.

---

## 3. Test Coverage

### Counts

| Repo | Tests | Status | Notes |
|------|-------|--------|-------|
| Exoskeleton (Rust) | ~1,020+ | All pass | 7 crates + 47 acceptance tests |
| WorldInterface (Rust) | ~631 | All pass | 10 crates + integration tests |
| Observatory (Rust) | 57/58 | **1 flaky** | `ensure_admin_without_env_var_writes_token` |
| Observatory (TypeScript) | 429 | All pass | 97 test files, ~18.9s |
| ActionQueue (Rust) | 939 (all features) | All pass | 49 acceptance + 1 chaos test |

### Strengths

- **Property-based testing** (proptest) in exoskeleton-core, WI contextstore, WI core
- **47 acceptance tests** in Exoskeleton covering all 9 sacred invariants
- **1 chaos test** in ActionQueue exercising WAL recovery under process termination
- **429 TypeScript tests** — every Observatory React component has a `__tests__/` counterpart (97 test files for 224 source files)
- **Mock infrastructure**: `MockLlmBackend`, `MockDockerManager`, `mockito` for HTTP

### Coverage Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No coverage metrics tool (cargo-llvm-cov, tarpaulin) | **Medium** | Flying blind on which code paths are exercised |
| exoskeleton-cli: 18 tests for 23 files (0.78 tests/file) | **Medium** | User-facing tool undertested |
| exoskeleton-daemon: 3 tests for 8 files (0.375 tests/file) | **Medium** | HTTP endpoints undertested |
| worldinterface-coordinator: 12 tests for complex orchestration | **Medium** | Integration-heavy, difficult to unit test |
| Observatory Rust backend: 58 tests for full auth+fleet+docker+bootstrap+proxy | **Low** | Adequate but light |
| Observatory flaky test: `ensure_admin_without_env_var_writes_token` | **Low** | Setup token file creation race condition |

---

## 4. Code Quality

### Universal Strengths (All Repos)

| Metric | Status |
|--------|--------|
| `unsafe {}` blocks | **Zero** across all 4 repos |
| `todo!()` / `unimplemented!()` | **Zero** across all 4 repos |
| `TODO` / `FIXME` / `HACK` comments | **Zero** across all 4 repos |
| Module-level `//!` docs | **Every** lib.rs and kernel module file |
| Error types | Consistent `thiserror` with typed enums |
| Instrumentation | Consistent `tracing` |
| `unwrap()` discipline | Production code: only on provably safe ops (NonZeroUsize, etc.) |

### Per-Repo Issues

**ActionQueue** — 2 clippy lint failures:
- `crates/actionqueue-core/src/run/run_instance.rs` lines 271, 325: `unnecessary_unwrap` — `.expect()` after `.is_some()` check. Should use `if let Some(id) = ...` pattern. **5-minute fix.**

**Exoskeleton** — Minor issues:
- Missing `#![forbid(unsafe_code)]` on all 7 crates (ActionQueue has it on all 11)
- Misleading function name `unsafe_send_kernel()` in `act.rs:717` — doesn't use `unsafe`, just clones Arcs
- Unusual serde constraint: `>=1.0, <1.0.224` — needs documentation on why 1.0.224+ is blocked
- ts-rs macro warnings (19 benign warnings about `#[serde(transparent)]`)

**WorldInterface** — Clean. Zero issues found.

**Observatory** — 1 flaky Rust test, otherwise clean. Strict TypeScript: only 1 `any` type usage across entire SPA.

---

## 5. Repository Documentation

### README Assessment

| Repo | Status | Quality | Issues |
|------|--------|---------|--------|
| Exoskeleton | Exists | **Excellent** (293 lines) | Test count outdated (~997 → should be ~1,020+). Endpoint count likely outdated post-E5. |
| Observatory | **MISSING** | N/A | Has excellent CLAUDE.md but no public-facing README |
| WorldInterface | Exists | **Good** | Missing `worldinterface-wasm` from crate table. Connector list outdated (only shows 4 of 7 native + 5 WASM). |
| ActionQueue | Exists | **Excellent** (189 lines) | Accurate test count (939). Comprehensive feature docs. |

### docs/ Directory Assessment

| Repo | Files | Quality | Gaps |
|------|-------|---------|------|
| Exoskeleton | 5 docs | Good | Missing: E5 features guide, PODAARA step reference, thread dev guide |
| WorldInterface | 4 docs | Good | Missing: WASM connector development guide, WIT interface ref, transform types, streaming API |
| ActionQueue | 10 docs | **Excellent** | Charter, invariants, guardrails, policy defaults, WAL recovery, getting started |
| Observatory | **0 docs** | N/A | No docs directory at all |

### Inline Documentation

Consistently excellent across all repos. Every lib.rs has `//!` with purpose and invariant references. Kernel PODAARA files all documented. Public APIs well-documented.

---

## 6. CI/CD Pipeline

### Pipeline Summary

| Repo | Jobs | Notes |
|------|------|-------|
| Exoskeleton | fmt, clippy, test, doc, docker (5 jobs) | Checks out WI + AQ as dependencies. Uses `--no-default-features`. |
| Observatory | SPA (type-check+lint+test+build), fmt, clippy, test, docker (5 jobs) | Full TS validation. Creates placeholder dist/ for rust-embed. |
| WorldInterface | fmt, clippy, test (3 jobs) | Includes daemon acceptance tests as separate step. |
| ActionQueue | fmt, clippy, test x8 feature combos, serde tests (1 job, 17 steps) | Tests every feature flag combination. Most thorough CI. |

### CI Issues

1. **Toolchain version skew**: WorldInterface CI uses **1.86.0** while Exoskeleton and ActionQueue use **1.89.0**. Observatory uses 1.89.0. WorldInterface should be updated.

2. **Exoskeleton CI uses `--no-default-features`**: Clippy and tests run without default features. If there are feature-gated code paths, they aren't validated.

3. **No cross-repo integration CI**: No workflow validates the full Exo + WI + Observatory stack together. Each repo validates independently (Exo checks out WI+AQ, Observatory checks out Exo).

---

## 7. Architecture Assessment

### Things Done Exceptionally Well

1. **Dual-engine architecture (I9)**: Cleanest separation of cognitive and tool execution in the codebase. Independent WALs, schedulers, budgets, dispatch loops. Act step as sole boundary crossing.

2. **Multi-turn Decide with introspection (E5)**: Demand-driven querying within iterative reasoning — the standard ReAct pattern applied to self-awareness. Architecturally elegant.

3. **WASM capability model**: Deny-by-default with sidecar manifests, separate resource pools per module, fuel metering, hostname pattern matching, filesystem path prefix validation, command allowlists. Production-grade sandboxing.

4. **ActionQueue's contract model**: WAL v5, deterministic recovery, validated state transitions, lease-based execution, in-flight cancellation. The 49 acceptance tests + chaos test provide real confidence.

5. **Observatory decoupling**: Pure HTTP/WebSocket boundary. Trait-based Docker abstraction (`DockerManager`). Could theoretically manage non-Exoskeleton agents implementing the same API.

### Areas for Attention

1. **Roadmap v2 is stale**: Doesn't reflect AQ's platform features. Epoch 7 scope overestimated. E5 not marked complete. Test counts outdated. Capability catalog outdated.

2. **No schema migration story**: 8 SQLite stores (Exo), 1 contextstore (WI), 1 fleet store (Obs), AQ WAL v5 + snapshots v8. No migration tooling (sqlx, refinery, manual scripts). Critical before production deployment.

3. **Configuration fragmentation**: `vessel.toml` (EXO_*), `config.toml` (WI_*), `observatory.toml` (no prefix), AQ daemon config. Four config paradigms.

4. **Floating items never addressed**: W-31 (artifact full-text search), W-32 (cross-tick snapshot comparison), W-38 (fleet topology) have floated since roadmap creation.

---

## 8. Prioritized Recommendations

### P0 — Do First (Low Effort, High Impact)

| # | Action | Effort | Impact |
|---|--------|--------|--------|
| 1 | **Create Observatory README.md** | 30 min | High — most user-visible gap |
| 2 | **Add `#![forbid(unsafe_code)]` to Exo + WI crates** | 10 min | High — consistency with AQ |
| 3 | **Fix 2 AQ clippy lint failures** (run_instance.rs:271,325) | 5 min | Blocks AQ CI |
| 4 | **Update Exo README test count** (~997 → ~1,020+) and endpoint count | 10 min | Accuracy |
| 5 | **Update WI README** to include wasm crate + current connector list | 15 min | Accuracy |
| 6 | **Update WI CI toolchain** from 1.86.0 → 1.89.0 | 5 min | Consistency |

### P1 — Do Soon (Medium Effort, High Impact)

| # | Action | Effort | Impact |
|---|--------|--------|--------|
| 7 | **Revise Roadmap to v3** — mark E5 complete, update AQ platform features, revise E7 scope, update test counts and capability catalog | 2 hours | High — roadmap is primary planning artifact |
| 8 | **Fix Observatory flaky test** (`ensure_admin_without_env_var_writes_token`) | 30 min | Low-medium |
| 9 | **Add coverage tooling** (cargo-llvm-cov or tarpaulin) | 1 hour | Medium — know your baseline |
| 10 | **Document serde constraint** (`>=1.0, <1.0.224`) in Exo workspace Cargo.toml | 10 min | Clarity |

### P2 — Do Eventually (Higher Effort)

| # | Action | Effort | Impact |
|---|--------|--------|--------|
| 11 | **Schema migration strategy** for all SQLite stores | 4 hours | Critical before production |
| 12 | **WASM connector developer guide** in WI docs | 3 hours | Enables external contributors |
| 13 | **Expand CLI/daemon test coverage** (0.78 and 0.375 tests/file) | 4 hours | Medium |
| 14 | **Cross-repo integration CI** | 3 hours | Medium |
| 15 | **Rename `unsafe_send_kernel`** → `clone_kernel_for_blocking` | 5 min | Minor clarity |
| 16 | **Address floating work items** (W-31, W-32, W-38) — schedule or explicitly defer | 30 min | Planning clarity |

---

## 9. Per-Repo Summary Cards

### Exoskeleton
- **Grade: A** | 7 crates, ~43K LOC, ~1,020+ tests
- **Strengths**: Pure core, dual-engine architecture, 47 acceptance tests, comprehensive docs
- **Issues**: Missing `#![forbid(unsafe_code)]`, outdated README metrics, light CLI/daemon coverage

### Observatory
- **Grade: A-** | 1 Rust crate + React SPA, ~28K LOC, 487 tests
- **Strengths**: Clean decoupling, trait-based Docker abstraction, strict TypeScript (1 `any`), comprehensive component testing
- **Issues**: No README.md, no docs directory, 1 flaky test

### WorldInterface
- **Grade: A** | 10 crates, ~631 tests
- **Strengths**: Zero coupling to Exoskeleton, comprehensive WASM runtime, deny-by-default capability model, clean clippy
- **Issues**: Missing `#![forbid(unsafe_code)]`, outdated README, low coordinator test count, CI toolchain skew (1.86.0 vs 1.89.0)

### ActionQueue
- **Grade: A** | 11 crates, 939 tests (all features)
- **Strengths**: `#![forbid(unsafe_code)]` on all crates, deterministic WAL recovery, chaos testing, platform/RBAC/actor already built, 10 design docs, most thorough CI (every feature combo)
- **Issues**: 2 clippy lint failures (trivial fix)

---

## 10. Sacred Invariants Verification

All 9 invariants verified as implemented and tested:

| ID | Invariant | Status | Evidence |
|----|-----------|--------|----------|
| I1 | No external state changes outside adapters | **Enforced** | LLM calls via CognitiveEngine, all effects through Act→WI Host |
| I2 | Every action gets run_id (idempotent) | **Enforced** | ActionQueue TaskId on all tool invocations |
| I3 | Everything replayable from WALs + artifacts | **Enforced** | 8 SQLite stores + 2 WALs, full tick records |
| I4 | Least privilege by default | **Enforced** | API keys env-only, WASM deny-by-default, Align gating |
| I5 | Context compiled, not accumulated | **Enforced** | Token-budgeted ContextCompiler, functional, stateless |
| I6 | Budgets enforced independently per engine | **Enforced** | CognitiveBudgetTracker + ToolBudgetGate, two-layer |
| I7 | Single coherent workspace | **Enforced** | One StateSnapshot per tick, master loop authority |
| I8 | Relationship awareness durable and explicit | **Enforced** | Append-only RelationshipLedger, immutable events |
| I9 | Cognitive and tool execution isolated | **Enforced** | Dual engines, independent WALs/schedulers, Act as sole crossing |

---

## 11. Metrics at Audit Time

| Metric | Value |
|--------|-------|
| Total Rust crates | 28+ (7 Exo + 10 WI + 11 AQ + 1 Obs) |
| Total tests | ~3,100+ (with AQ all features) |
| Total LOC (Rust) | ~100K+ |
| Total LOC (TypeScript) | ~23K |
| Unsafe blocks | 0 |
| TODO/FIXME markers | 0 |
| Clippy violations | 2 (AQ only, trivial) |
| CI pipelines | 4 (all repos) |
| Acceptance tests | 47 (Exo) + 49 (AQ) = 96 |
| Property-based tests | proptest in Exo core, WI core, WI contextstore |
| Chaos tests | 1 (AQ WAL recovery) |
| WASM connectors | 5 (json-validate, streaming-echo, webhook.send, web.search, discord) |
| Native connectors | 7 (delay, http.request, fs.read, fs.write, shell.exec, sandbox.exec, peer.resolve) |
| Transform types | 6 (Identity, FieldMapping, Filter, StringTemplate, ArrayFlatten, ArrayMap) |
| Observatory panels | 9 (Dashboard, Chat, Timeline, Threads, Relationships, Memory, Artifacts, Fleet, Auth) |
