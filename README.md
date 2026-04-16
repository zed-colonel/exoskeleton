# Exoskeleton

## Overview

**Exoskeleton** is a persistent, governable AI agent runtime built in Rust.

It turns an LLM into a **durable autonomous agent** with structured cognition, relationship awareness, and budget-controlled execution. The core abstraction is the **Vessel** -- a long-running process that perceives its environment, reasons through cognitive threads, makes trust-gated decisions, acts through tool adapters, and reflects on outcomes. All state is persisted and replayable.

Exoskeleton operates **two physically separate ActionQueue engines** (invariant I9). The **Cognitive AQ** owns the master loop, thread executions, and LLM inference. The **Tool AQ** (owned by the WorldInterface Host) owns adapter invocations and external I/O. These engines have independent WALs, schedulers, budgets, and dispatch loops. The **Act step** is the sole boundary crossing from cognitive decisions to tool execution.

> Exoskeleton is meant to be a point of leverage through which cognition becomes governed action.

---

## Features

### PODAARA Master Loop

- **Perceive** -- drain inbox, detect new signals, load prior state
- **Orient** -- compile context window from snapshot, memory, relationships
- **Decide** -- LLM inference to produce a structured action plan
- **Align** -- trust-gated approval based on relationship history
- **Act** -- execute approved actions through the Tool AQ (sole boundary crossing, I9)
- **Reflect** -- evaluate outcomes against expectations
- **Amend** -- persist updated snapshot, tick record, events, artifacts

### Cognitive Threads

- **Threat Monitor** (Critical/EveryTick) -- adversarial prompt detection, capability abuse
- **Self-Critique** (High/EveryTick) -- reasoning quality assessment, uncertainty scoring
- **Memory Consolidation** (Normal/EveryNTicks(5)) -- episodic compression, long-term note extraction
- **Meta-Cognition** (Normal/EveryNTicks(10)) -- cognitive pattern analysis, charter modification proposals
- **Creative Synthesis** (Background/EveryNTicks(15)) -- novel cross-domain connections, hypothesis generation
- Thread outputs converge into a single StateSnapshot per tick
- Custom threads via `ThreadSpec` registration

### Relationship Substrate

- **Append-only RelationshipLedger** -- durable record of all relational signals
- **Trust computation** -- per-principal trust score from interaction history
- **Alignment gating** -- configurable trust thresholds for normal and destructive actions
- **Relationship snapshots** -- compiled per-principal summaries available to Orient

### Budget Enforcement

- **Two-layer enforcement** -- AQ BudgetGate (hard dispatch stop) + Exoskeleton CognitiveBudgetTracker (fine-grained)
- **Independent budgets** -- cognitive and tool budgets enforced separately (I6, I9)
- **Per-window replenishment** -- configurable time windows with automatic reset
- **Thrash detection** -- graduated response to action repetition and stagnation
- **Model escalation** -- automatic local-to-frontier promotion on consecutive failures

### Memory

- **Context Compiler** -- token-budgeted context window assembly (I5: compiled, not accumulated)
- **Episodic summaries** -- compressed tick-range narratives
- **Long-term notes** -- persistent topic-keyed knowledge

### LLM Integration

- **Local models** -- Ollama, vLLM, LM Studio, llama.cpp via OpenAI-compatible API
- **Frontier models** -- Anthropic (Messages API), OpenAI (Chat Completions)
- **API keys from env vars** -- never stored in config, artifacts, or logs (I4)
- **Configurable escalation** -- uncertainty-based or failure-based promotion to frontier

### Observability

- **HTTP daemon** -- axum-based REST API with 35 endpoints + WebSocket
- **CLI** -- `exo` binary with `bootstrap`, `start`, `inspect`, `thread`, `relationship`, `budget`, `events`, `engines`, `send`, `artifact`, `memory`, `snapshots`, `inbox-history`, `config`, `reload-charters`, `fork` commands
- **Docker** -- multi-stage build, compose profiles for single and multi-vessel
- **Observatory** -- fleet management UI and vessel orchestration ([separate project](https://github.com/zed-colonel/observatory))
- **Prometheus metrics** -- `exo_ticks_total`, `exo_tick_duration_seconds`, `exo_current_tick_number`
- **8 SQLite stores** -- independently queryable, WAL-mode, backup-friendly

---

## Crate Architecture

Seven workspace crates forming a strict dependency DAG:

```
  exoskeleton-core           Pure domain types (no I/O, no AQ/WI dep) -- leaf of DAG
  exoskeleton-memory         Context Compiler + memory tiers
  exoskeleton-relationship   Relationship Ledger + Snapshot + Align
  exoskeleton-threads        Thread registry, execution, context slicing, built-in threads
  exoskeleton-host           Vessel: dual-engine boot, master loop, LLM handler, storage
  exoskeleton-daemon         HTTP daemon (axum), REST API, Prometheus metrics
  exoskeleton-cli            CLI binary (clap) -- `exo`
```

Dependency flow:

```
  exoskeleton-cli
       |
  exoskeleton-daemon
       |
  exoskeleton-host
      /|\
     / | \
    /  |  \
  threads  memory  relationship
    \  |  /
     \ | /
      \|/
  exoskeleton-core
```

External dependencies:

- **ActionQueue** (`actionqueue-*` crates) -- cognitive engine dispatch, WAL, scheduling, budgets
- **WorldInterface** (`wi-*` crates) -- tool adapter framework, connector registry, Tool AQ host

---

## Quick Start

### As an embedded library

```rust
use exoskeleton_host::config::VesselConfig;
use exoskeleton_host::vessel::Vessel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = VesselConfig {
        mission: "Monitor and respond to incoming requests".into(),
        data_dir: "./data".into(),
        ..Default::default()
    };

    let vessel = Vessel::start(config).await?;

    // Vessel is now running: master loop ticking, threads executing,
    // both engines independently processing their work queues.

    // Use the inspector for read-only queries:
    let inspector = vessel.inspector();

    // Invoke tools through the WI Host (Tool AQ path):
    let result = vessel.invoke_tool("delay", serde_json::json!({"ms": 100})).await?;

    vessel.shutdown().await?;
    Ok(())
}
```

### Bootstrap + Run (local)

```bash
# 1. Bootstrap a new vessel (interactive wizard + first-contact conversation)
export ANTHROPIC_API_KEY=sk-ant-...   # or OPENAI_API_KEY
exo bootstrap

# 2. Start the vessel
exo start --config ~/.exo/vessels/default/vessel.toml
```

The bootstrap wizard configures the LLM backend, verifies connectivity, and runs
a first-contact conversation where you and the agent establish its name, purpose,
and working relationship. The conversation becomes the agent's origin story.

### Docker

```bash
# Build the image
docker build -f docker/Dockerfile.vessel -t exoskeleton:latest .
```

**Option A: Bootstrap locally, run in Docker**

```bash
# Bootstrap on the host (writes config + data to a local directory)
exo bootstrap

# Run the container with the bootstrapped data
docker run -d \
  -v ~/.exo/vessels/default:/data \
  -p 7600:7600 \
  -e OPENAI_API_KEY \
  --user "$(id -u):$(id -g)" \
  exoskeleton:latest
```

**Option B: Bootstrap inside Docker**

```bash
# Bootstrap inside the container (interactive — needs -it)
docker run -it --rm \
  -v ~/.exo/my-vessel:/data \
  -e OPENAI_API_KEY \
  --user "$(id -u):$(id -g)" \
  exoskeleton:latest bootstrap --data-dir /data

# Then start the vessel
docker run -d \
  -v ~/.exo/my-vessel:/data \
  -p 7600:7600 \
  -e OPENAI_API_KEY \
  --user "$(id -u):$(id -g)" \
  exoskeleton:latest
```

> **Volume permissions:** The `--user "$(id -u):$(id -g)"` flag ensures the
> container can write to SQLite databases in the mounted volume. Without it,
> the default container user may lack write access.

### CLI

Once a vessel is running (locally or in Docker), interact with it via the CLI:

```bash
exo inspect                        # Current state snapshot
exo inspect ticks --limit 10       # Recent PODAARA tick history
exo send "Hello, vessel"           # Send a message (picked up next tick)
exo thread list                    # View cognitive threads
exo relationship show              # View relationship snapshot
exo budget                         # View budget status
exo events --limit 50              # Recent events
exo engines                        # Dual engine health
```

### Build

```bash
cargo build --workspace                               # Build all crates
cargo test --workspace                                # Run all tests (~1,100)
cargo clippy --all --all-targets -- -D warnings       # Lint (strict)
cargo fmt --all -- --check                            # Format check
cargo doc --workspace --no-deps                       # Build docs
```

### Install `exo` To User `bin/`

```bash
./scripts/install-exo-user-bin.sh          # Build release and install to ~/bin or ~/.local/bin
./scripts/install-exo-user-bin.sh debug    # Install a debug build instead
```

The installer prefers `~/bin` when it exists, otherwise it installs to
`~/.local/bin`. Set `EXO_USER_BIN_DIR` to override the destination.

---

## Sacred Invariants

Nine governing principles enforced across all crates:

| ID | Invariant | Enforcement |
|----|-----------|-------------|
| I1 | No external state changes outside adapters | LLM calls are cognitive work, not tool use |
| I2 | Every external action gets `run_id` (idempotent) | ActionQueue run lifecycle |
| I3 | Everything replayable from both AQ WALs + artifacts | 8 SQLite stores + 2 WALs |
| I4 | Least privilege by default | API keys from env vars, never stored |
| I5 | Context compiled, not accumulated | Token-budgeted Context Compiler |
| I6 | Budgets enforced independently per engine | Two-layer: AQ BudgetGate + Exo trackers |
| I7 | Single coherent workspace | One StateSnapshot, one master loop |
| I8 | Relationship awareness is durable and explicit | Append-only RelationshipLedger |
| I9 | Cognitive and tool execution isolated | Dual-engine, never merge |

---

## Testing

~1,100 Rust tests across unit, integration, and acceptance levels:

- **Unit tests** -- in-crate `#[cfg(test)] mod tests` blocks
- **Integration tests** -- multi-crate interaction tests in `exoskeleton-host`
- **Acceptance tests** (8 test files, 47 tests) -- full Vessel boot with mock LLM:
  - **A: Thread Convergence** -- 3 threads converge into 1 StateSnapshot
  - **B: Kill/Restart Durability** -- relationship and tick state survives crash/restart
  - **C: Thread Replay** -- artifact chain from TickRecord to ThreadOutput to LlmResponse
  - **D: Atomic Align** -- ledger coherence preserved through crash
  - **E: Budget Enforcement** -- cognitive and tool budgets enforced independently
  - **F: Inspection Surface** -- daemon endpoints show full vessel state
  - **G: Engine Isolation** -- cognitive ticks unaffected by Tool AQ saturation

See [`docs/acceptance-test-taxonomy.md`](docs/acceptance-test-taxonomy.md) for detailed invariant mappings.

---

## Configuration

Configuration is TOML-based (`vessel.toml`) with environment variable overrides.

See [`docs/configuration.md`](docs/configuration.md) for the full reference.

---

## Documentation

- [Architecture reference](docs/architecture.md)
- [Configuration reference](docs/configuration.md)
- [Acceptance test taxonomy](docs/acceptance-test-taxonomy.md)
- [Data directory layout](docs/data-directory.md)
- [Relationship demo walkthrough](docs/examples/relationship-demo.md)
- [Project charter](exoskeleton_charter_1.0.md)
- [Invariant boundaries policy](exoskeleton_invariant_boundaries_policy_1.0.md)
- [Scope appendix](exoskeleton_scope_appendix_1.0.md)

---

## License

This project is licensed under the GNU Affero General Public License v3.0 -- see the [LICENSE](LICENSE) file for details.
