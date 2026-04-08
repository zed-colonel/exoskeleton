# SWE-bench Adapter Design

> **Pre-E11** — Native ingestion of SWE-bench format datasets for Rust coding
> benchmarks, with self-contained evaluation and predictions export.

**Goal:** Enable `exo bench` to run against standard SWE-bench datasets (Rust-SWE-bench,
SWE-bench Multilingual) without manual TOML conversion. The adapter fetches datasets
from HuggingFace, clones and caches repos, boots a Vessel per task, extracts the
agent's diff, and grades against `FAIL_TO_PASS`/`PASS_TO_PASS` test lists. A predictions
JSONL export supports official Docker-based evaluation when publishable numbers are needed.

**Architecture:** New `swe_bench` module in `exoskeleton-host` with `SweBenchRunner`
orchestrator. Shares vessel execution core with the existing `HeadlessRunner` via
extracted helper functions. Three submodules: `dataset` (HuggingFace REST API fetch +
local cache), `repo` (bare clone cache + git worktree per task), `eval` (patch
application, cargo test parsing, F2P/P2P grading). CLI gains `--swe-bench` flag
alongside existing TOML mode.

---

## 1. Data Model

### SweInstance

A single SWE-bench task, deserialized from the HuggingFace JSON API:

```rust
pub struct SweInstance {
    pub instance_id: String,       // e.g., "tokio-rs__tokio-4384"
    pub repo: String,              // e.g., "tokio-rs/tokio"
    pub base_commit: String,       // 40-char SHA
    pub problem_statement: String, // Issue text — the agent's prompt
    pub hints_text: String,        // Issue comments (not provided to agent by default)
    pub patch: String,             // Gold solution diff (never shown to agent)
    pub test_patch: String,        // Test additions applied before grading
    pub fail_to_pass: Vec<String>, // Tests that must go from fail to pass
    pub pass_to_pass: Vec<String>, // Tests that must stay passing
}
```

Both Rust-SWE-bench (`user2f86/rustbench`) and SWE-bench Multilingual share these
core fields. Rust-SWE-bench has extra fields (`pull_number`, `issue_numbers`,
`FAIL_TO_FAIL`, `PASS_TO_FAIL`, `source_dir`) that we ignore. Both datasets use
native lists for `FAIL_TO_PASS`/`PASS_TO_PASS` (unlike the original Python SWE-bench
which JSON-encodes them as strings).

### SweResult

Extends `TaskResult` with SWE-bench-specific grading:

```rust
pub struct SweResult {
    pub task_result: TaskResult,              // Steps, tokens, cost, etc.
    pub instance_id: String,
    pub model_patch: String,                  // Agent's diff for predictions export
    pub fail_to_pass_resolved: Vec<String>,   // Which F2P tests now pass
    pub pass_to_pass_maintained: Vec<String>, // Which P2P tests still pass
    pub grading: SweGrading,
}
```

### SweGrading

```rust
pub struct SweGrading {
    pub resolved: bool,  // All F2P pass AND all P2P still pass
    pub f2p_passed: u32,
    pub f2p_total: u32,
    pub p2p_passed: u32,
    pub p2p_total: u32,
}
```

A task is `resolved` only when every `FAIL_TO_PASS` test passes and every
`PASS_TO_PASS` test remains passing. This matches the official SWE-bench grading
criteria.

---

## 2. Module Structure

### New modules in `exoskeleton-host`

**`src/swe_bench/mod.rs`** — Public API, type definitions, `SweBenchRunner`.

**`src/swe_bench/dataset.rs`** — Dataset fetching and caching:
- `DatasetSource` enum: `RustBench` or `Multilingual`
- `fetch_dataset(source, cache_dir) -> Result<Vec<SweInstance>>` — HuggingFace
  Rows REST API with pagination (100 rows/page). Multilingual is filtered to Rust
  instances at load time.
- Fetched data cached as JSONL at `~/.cache/exo-bench/datasets/{name}.jsonl`.
  Subsequent runs read from cache unless `--refresh` is passed.

**`src/swe_bench/repo.rs`** — Repository clone cache and worktree management:
- `RepoCache` struct wrapping a cache directory (default `~/.cache/exo-bench/repos/`).
- `ensure_repo(repo) -> Result<PathBuf>` — bare `git clone --bare` if not cached,
  `git fetch` if cached.
- `prepare_worktree(repo, commit) -> Result<TempDir>` — creates a git worktree at
  the specified commit in a tempdir. The tempdir cleans up on drop (including
  `git worktree remove`).

**`src/swe_bench/eval.rs`** — Patch application, test execution, grading:
- `apply_patch(workspace, patch_text) -> Result<()>` — `git apply` with fallback
  to `git apply --3way`.
- `run_cargo_test(workspace, timeout) -> Result<CargoTestOutput>` — runs
  `cargo test --no-fail-fast`, captures stdout/stderr, respects timeout.
- `parse_cargo_test_output(output) -> HashMap<String, TestOutcome>` — parses
  `test path::to::name ... ok/FAILED/ignored` lines.
- `grade(parsed, f2p, p2p) -> SweGrading` — exact match with suffix fallback.

### Extracted helpers from `benchmark.rs`

The following functions are extracted from `HeadlessRunner` into module-level
functions so both `HeadlessRunner` and `SweBenchRunner` can call them:
- `boot_vessel(config) -> Result<Vessel>`
- `inject_and_poll(vessel, prompt, timeout) -> Result<(String, VesselInspector)>`
- `extract_metrics(inspector) -> Result<MetricsSnapshot>`

`HeadlessRunner::run_task()` is refactored to call these instead of inlining the
logic. Its public API does not change.

### CLI changes in `exoskeleton-cli`

New flags on `exo bench`:
- `--swe-bench <dataset>` — `rustbench` or `multilingual` (mutually exclusive with
  `--task`/`--suite`)
- `--swe-limit <N>` — cap the number of instances to run
- `--swe-instance <id>` — run a single instance by ID
- `--export-predictions <path>` — write predictions JSONL after the run
- `--refresh` — re-fetch the dataset from HuggingFace (ignore local cache)

All existing flags compose with SWE-bench mode: `--config`, `--record`, `--compare`,
`--verbose`, `--provider`, `--model`, `--api-key-env`, `--local-endpoint`,
`--dry-run`, `--results-dir`.

---

## 3. SweBenchRunner Lifecycle

Per-instance execution flow:

```
SweBenchRunner::run(config, dataset, options)
  │
  ├─ fetch_dataset(source)           — HuggingFace REST or local cache
  │
  └─ for each SweInstance:
       │
       ├─ 1. ensure_repo(repo)       — bare clone or git fetch
       ├─ 2. prepare_worktree(commit) — tempdir at base_commit
       ├─ 3. build_vessel_config()    — workspace root = worktree path
       ├─ 4. boot_vessel(config)      — shared helper
       ├─ 5. inject prompt            — problem_statement with workspace context
       ├─ 6. poll_for_completion()    — shared helper
       ├─ 7. extract_metrics()        — shared helper
       ├─ 8. shutdown vessel
       │
       │  ── Agent done. Now evaluate. ──
       │
       ├─ 9. extract diff             — `git diff` in worktree (agent's patch)
       ├─ 10. apply test_patch        — `git apply` the SWE-bench test additions
       ├─ 11. run cargo test          — `cargo test --no-fail-fast` with timeout
       ├─ 12. parse + grade           — compare against F2P/P2P lists
       │
       └─ → SweResult
```

**Steps 3-8** use shared helpers extracted from `HeadlessRunner`.

**Step 9** runs before step 10 — we capture the clean agent diff before applying
the test patch. This diff is what gets exported in predictions JSONL.

**Step 5 prompt:** The agent receives the `problem_statement` prefixed with workspace
context (same pattern as `format_task_prompt()`). `hints_text` is not included by
default.

**Worktree cleanup:** The tempdir is preserved on failure (same as HeadlessRunner)
for post-mortem debugging.

---

## 4. Dataset Fetching

### HuggingFace Rows REST API

No authentication required. Paginated, 100 rows per page:

```
GET https://datasets-server.huggingface.co/rows
    ?dataset=user2f86/rustbench
    &config=default
    &split=train
    &offset=0
    &length=100
```

Response:

```json
{
  "features": [...],
  "rows": [
    { "row_idx": 0, "row": { "instance_id": "...", "repo": "...", ... } },
    ...
  ],
  "num_rows_total": 500
}
```

### Multilingual filtering

SWE-bench Multilingual contains 300 tasks across 9 languages. We filter to Rust by
checking whether `instance_id` corresponds to a known Rust repo. The 7 Rust repos in
the dataset are: `astral-sh/ruff`, `burntsushi/ripgrep`, `nushell/nushell`,
`sharkdp/bat`, `tokio-rs/axum`, `tokio-rs/tokio`, `uutils/coreutils`. This yields
43 Rust instances.

### Local caching

Fetched datasets are stored as JSONL at:
- `~/.cache/exo-bench/datasets/rustbench.jsonl`
- `~/.cache/exo-bench/datasets/multilingual-rust.jsonl`

The `--refresh` flag forces a re-fetch. Without it, cached data is used if present.

---

## 5. Repository Management

### Bare clone cache

Repos are cached as bare clones at `~/.cache/exo-bench/repos/{owner}/{repo}.git`.
A bare clone contains the full history without a working directory, making it
disk-efficient. On first use, `git clone --bare https://github.com/{owner}/{repo}.git`.
On subsequent use, `git fetch --all` to pick up any new commits.

Overridable via `--repos-cache <path>`.

### Git worktrees

Each task instance gets an isolated worktree via `git worktree add <tempdir> <commit>`.
This is fast (no network, no full clone) and provides a clean working directory at
exactly the right commit. The worktree is created in a tempdir that cleans up on drop.

On drop, we run `git worktree remove <path>` before the tempdir deletes. If the
worktree remove fails (task failure, post-mortem preservation), the bare repo's
worktree list may accumulate stale entries — `git worktree prune` on next
`ensure_repo()` handles this.

---

## 6. Self-Contained Evaluation

### Patch application

After the agent finishes and we've captured its diff (step 9), we apply the
SWE-bench `test_patch` to add the expected tests:

```
git apply --verbose <test_patch>
```

Fallback on failure: `git apply --3way <test_patch>` (attempts three-way merge).
If both fail, the instance is graded as Failed (test setup error).

### Cargo test execution

```
cargo test --no-fail-fast 2>&1
```

`--no-fail-fast` ensures all tests run even if some fail (we need the full picture
for P2P checking). Output is captured as a string for parsing.

Timeout: default 300 seconds, configurable via `--swe-test-timeout`. On timeout,
the process is killed and the instance is graded as Failed.

### Output parsing

Cargo test output format:

```
test path::to::test_name ... ok
test path::to::other_test ... FAILED
test path::to::skipped ... ignored
```

The parser extracts `(test_name, outcome)` pairs using a simple regex:
`^test ([\S]+) \.\.\. (ok|FAILED|ignored)$`

`TestOutcome` enum: `Passed`, `Failed`, `Ignored`.

### Grading

For each test in `FAIL_TO_PASS`:
- Look up in parsed results (exact match, then suffix fallback)
- Must be `Passed` to count as resolved

For each test in `PASS_TO_PASS`:
- Look up in parsed results (exact match, then suffix fallback)
- Must be `Passed` to count as maintained
- If not found in output, treated as maintained (test may have been filtered
  out by `cargo test` selection)

Final grading:
- `resolved = (f2p_passed == f2p_total) && (p2p_passed == p2p_total)`

### Compilation failure

If `cargo test` exits with a non-zero code and the output contains no
`test ... ok` or `test ... FAILED` lines, we treat this as a compilation failure:
`f2p_passed = 0, p2p_passed = 0, resolved = false`.

---

## 7. Predictions Export

For official evaluation, we export predictions JSONL:

```jsonl
{"instance_id": "tokio-rs__tokio-4384", "model_name_or_path": "exoskeleton-v0.1", "model_patch": "diff --git a/..."}
```

The `--export-predictions <path>` flag writes this file after the run completes.
Each `SweResult.model_patch` (captured at step 9) becomes a line. The
`model_name_or_path` is derived from the model name in the vessel config.

This file can be fed directly to the official SWE-bench Docker harness for
evaluation when publishable numbers are needed.

---

## 8. CLI Interface

```
# Run all Rust-SWE-bench tasks
exo bench --swe-bench rustbench --config benchmarks/bench-vessel.toml

# Run Multilingual (Rust subset only)
exo bench --swe-bench multilingual --config benchmarks/bench-vessel.toml

# Single instance for debugging
exo bench --swe-bench rustbench --swe-instance tokio-rs__tokio-4384 \
  --config benchmarks/bench-vessel.toml --verbose

# Development iteration: first 10 tasks
exo bench --swe-bench rustbench --swe-limit 10 \
  --config benchmarks/bench-vessel.toml --verbose

# Export predictions for official evaluation
exo bench --swe-bench rustbench --config benchmarks/bench-vessel.toml \
  --export-predictions predictions.jsonl

# Record and compare (same as TOML mode)
exo bench --swe-bench rustbench --config benchmarks/bench-vessel.toml \
  --record swe-rustbench-v1 --verbose

# Model override (same flags as TOML mode)
exo bench --swe-bench rustbench --config benchmarks/bench-vessel.toml \
  --provider openai --model gpt-4o
```

Mutually exclusive: `--swe-bench` vs `--task`/`--suite`. Error if both provided.

---

## 9. Scope & Boundaries

### In scope

- `SweInstance`, `SweResult`, `SweGrading` types
- `SweBenchRunner` orchestrator
- HuggingFace REST API dataset fetching (Rust-SWE-bench + Multilingual)
- Local dataset caching (JSONL)
- Bare clone repo cache with git worktree per task
- `cargo test` output parsing and F2P/P2P grading
- Predictions JSONL export
- CLI flags: `--swe-bench`, `--swe-limit`, `--swe-instance`, `--export-predictions`, `--refresh`
- Shared helper extraction from `HeadlessRunner`
- Report formatting for SWE-bench results (resolve rate, per-instance breakdown)

### Out of scope

- Docker-based evaluation (use official harness with exported predictions)
- Python SWE-bench support (different test runner, JSON-encoded fields)
- Non-Rust languages from Multilingual (deferred to post-LSP in E11)
- Per-repo custom cargo commands (some repos need feature flags — handle
  if encountered, don't pre-build a framework)
- Parallel task execution (E11 adds parallelization)
- HuggingFace authentication (both datasets are public)
