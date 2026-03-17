# Relationship Substrate Demo

A step-by-step walkthrough of Exoskeleton's relationship substrate -- trust-based
governance that tracks principals, builds trust from interaction history, and gates
actions based on trust levels.

**Prerequisites:**
- Exoskeleton built (`cargo build --workspace`)
- An LLM backend running (local via Ollama, or frontier API key)
- The `exo` binary on your PATH

**Time:** ~15 minutes

**What you'll see:**
- Per-principal trust scores evolving from message interaction
- Trust gating at the Align step (normal vs. destructive thresholds)
- Relationship state surviving a crash/restart
- Thread influence on relationship-aware cognition
- The relationship panel in Observatory

> **For developers:** If you want to embed the relationship substrate in your own Rust
> code (with a mock LLM, no external dependencies), see the programmatic demo at
> `examples/relationship-demo/src/main.rs`.

---

## Phase 1: Bootstrap and Start

### Option A: Interactive Bootstrap

```bash
exo bootstrap
```

Follow the wizard:
- **Data directory:** `./demo-data`
- **LLM backend:** Local (Ollama at `http://localhost:11434`, model `llama3.2:latest`)
- **Listen port:** 7600

The bootstrap wizard verifies LLM connectivity, runs a first-contact conversation,
and generates `./demo-data/vessel.toml`.

### Option B: Manual Configuration

Create `demo-vessel.toml`:

```toml
[vessel]
mission = "Relationship substrate demo -- track trust for multiple principals"
data_dir = "./demo-data"
master_loop_interval_secs = 30

[llm]
default_backend = "local"
max_output_tokens = 4096
timeout_secs = 120

[llm.local]
endpoint = "http://localhost:11434"
model = "llama3.2:latest"
api_format = "openai_compat"
```

### Start the Vessel

```bash
exo start --config ./demo-data/vessel.toml
# Or if bootstrapped:
exo start --config demo-vessel.toml
```

You should see:
```
INFO  vessel started: vessel_id=<uuid>, mission="Relationship substrate demo..."
INFO  daemon listening on 127.0.0.1:7600
INFO  Observatory UI available (embedded mode)   # If built with embedded feature
INFO  master loop started: interval=30s
```

Open a second terminal for CLI interaction.

---

## Phase 2: Send Messages from Different Principals

Each message is sent with a `--source` flag identifying the principal. UUIDs serve as
principal identifiers -- consistent UUIDs represent the same person across messages.

```bash
# Alice's messages
ALICE="11111111-1111-1111-1111-111111111111"
exo send "Hello! I'm Alice. I'm working on organizing the project documentation." --source $ALICE
exo send "Can you help me create an outline for the API reference?" --source $ALICE

# Bob's messages
BOB="22222222-2222-2222-2222-222222222222"
exo send "Hi, I'm Bob. I need help auditing our security configuration." --source $BOB
```

Wait for at least one PODAARA tick to process (default: 30 seconds, or check with
`exo inspect`). Then verify the messages were received:

```bash
exo inbox-history --limit 10
```

You should see 3 entries showing the messages from Alice and Bob with their
respective principal IDs.

```bash
exo events --limit 20
```

Look for `message_received` events -- one per submitted message.

---

## Phase 3: Observe Trust Evolution

After a few ticks, the relationship substrate has processed the messages and recorded
relational signals.

### View the Relationship Snapshot

```bash
exo relationship show
```

Expected output (approximate -- exact formatting may vary):

```
Relationship Snapshot
---------------------
Principals: 2

  11111111-...  (Alice)
    Trust:    0.54
    Signals:  3
    Last:     FeedbackReceived

  22222222-...  (Bob)
    Trust:    0.52
    Signals:  2
    Last:     FeedbackReceived
```

**How trust builds:**
- Each principal starts at **0.5** (neutral trust)
- `FeedbackReceived` signals add **+0.02** per occurrence
- `CommitmentFulfilled` signals add **+0.05**
- `CommitmentBroken` signals subtract **-0.15**
- `AlignmentMismatch` signals subtract **-0.05**
- Trust is clamped to [0.0, 1.0]

### View Relationship History

```bash
exo relationship history $ALICE --limit 20
```

This shows the append-only ledger entries for Alice -- every relational signal recorded,
with timestamps and artifact references.

### Send More Messages to Build Trust

```bash
exo send "The outline looks great, thank you! Very helpful." --source $ALICE
exo send "Good work on the security review." --source $BOB
exo send "Can you also check the deployment configuration?" --source $BOB
```

Wait for another tick cycle, then check again:

```bash
exo relationship show
```

Trust scores should have increased slightly as more positive interactions are recorded.

> **Tip:** Use `--json` with any command to get stable JSON output suitable for scripting
> or comparison across runs.

---

## Phase 4: Thread Influence on Relationship-Aware Cognition

### View Active Threads

```bash
exo thread list
```

Expected output:
```
Threads (3)
-----------
  Threat Monitor        Critical    EveryTick
  Self-Critique         High        EveryTick
  Memory Consolidation  Normal      EveryNTicks(5)
```

### Inspect Recent Tick Detail

```bash
exo inspect ticks --limit 5
```

Each tick shows thread contributions -- how each thread influenced the PODAARA cycle.

```bash
exo inspect tick <tick-id>
```

Look for:
- **Threat Monitor** -- assesses whether any messages contain adversarial patterns,
  capability requests outside trust levels, or anomalous behavior
- **Self-Critique** -- evaluates the quality of the vessel's reasoning and responses
  in the context of its relationships
- **Memory Consolidation** -- (runs every 5 ticks) compresses recent episodic memory,
  including relationship context, into long-term notes

### How Relationships Affect Cognition

The relationship snapshot is loaded during **Orient** and included in the compiled
context window. The LLM sees per-principal trust summaries when making decisions.

The **Align** step checks trust before approving actions:
- Normal actions require trust >= **0.3** (configurable)
- Destructive actions (e.g., `fs.write`) require trust >= **0.6** (configurable)
- A principal with trust below the threshold has their requested actions blocked

View the current state snapshot to see relationship data flowing through:

```bash
exo inspect
```

The `relationship_snapshot_ref` field links to the compiled RelationshipSnapshot
artifact that was active during the most recent tick.

See [Architecture Reference](../architecture.md) for details on the PODAARA cycle
and Align step.

---

## Phase 5: Kill and Restart -- Durability Proof

This demonstrates **invariant I8** (relationship awareness is durable and explicit).

### Record Pre-Crash State

```bash
# Note the current tick number
exo inspect | grep tick_number

# Note the relationship state
exo relationship show

# Note the event count
exo events --limit 1
```

### Simulate a Crash

In the terminal running the vessel:
```bash
# Hard kill -- no graceful shutdown
kill -9 $(pgrep -f "exo start")
```

Or simply Ctrl+C if running in the foreground (this is graceful, but the effect is
similar for durability testing).

### Restart

```bash
exo start --config ./demo-data/vessel.toml
```

### Verify State Survived

```bash
# Tick number should be >= pre-crash value
exo inspect | grep tick_number

# Relationship state should be identical
exo relationship show

# Relationship history should be intact
exo relationship history $ALICE --limit 20
```

**Why this works:**
- All 8 SQLite stores use **WAL journal mode** -- crash-safe, no corruption
- Relationship ledger is **append-only** -- no in-place updates to corrupt
- Cognitive AQ and Tool AQ recover **independently** from their own WALs (I9)
- Tick numbers resume **monotonically** from the latest completed tick
- Budget state is loaded from `budget.db` (or reset if the window expired)

See [Data Directory Layout](../data-directory.md) for the full persistence structure
and [Acceptance Test Taxonomy](../acceptance-test-taxonomy.md) for Criterion B
(Kill/Restart Durability).

### Send More Messages After Restart

```bash
exo send "I'm back -- did everything survive the restart?" --source $ALICE
```

Wait for a tick, then verify the vessel processes the message normally:

```bash
exo events --limit 10
```

You should see `vessel_started` followed by normal tick events and the new
`message_received`.

---

## Phase 6: Observatory View

If the vessel was built with the `embedded-observatory` feature (default), open
the Observatory UI in your browser:

```
http://localhost:7600/
```

> **Note:** If you built without the `embedded-observatory` feature, start Observatory
> separately via `npm run dev` in the `observatory/` directory, or use Docker Compose.

### Dashboard

The **Dashboard** panel shows:
- **Status card** -- vessel status, tick number, mission
- **Engine gauges** -- Cognitive AQ and Tool AQ health (I9 visual)
- **Event feed** -- live scrolling events including `relationship_updated` entries

### Relationships Panel

Navigate to **Relationships** in the sidebar.

**Relationship Snapshot** -- table of all known principals with:
- Principal ID (truncated UUID)
- Trust score (0.0--1.0)
- Signal count
- Last signal type and timestamp

**Relationship History** -- click a principal to see their full ledger history:
- Chronological list of every relational signal
- Signal type (FeedbackReceived, CommitmentFulfilled, CommitmentBroken, etc.)
- Tick reference and timestamp
- Artifact references for each signal

### Timeline Panel

Navigate to **Timeline** to see tick-by-tick PODAARA cycle execution:
- Each tick shows thread contributions
- Click a tick to see its **Context Breakdown** -- per-section token allocation
  including the relationship snapshot section

### Memory Panel

Navigate to **Memory** to see:
- **Episodic summaries** -- recent tick digests (may reference relationship events)
- **Long-term notes** -- consolidated knowledge (written by Memory Consolidation thread)

### Artifacts Panel

Navigate to **Artifacts** and filter by `RelationshipSnapshot` kind to see the
compiled snapshots stored as content-addressed artifacts (I3).

---

## Cleanup

Stop the vessel (Ctrl+C or `kill $(pgrep -f "exo start")`), then remove the data:

```bash
rm -rf ./demo-data
```

---

## Key Takeaways

1. **Relationships are durable (I8)** -- the append-only ledger survives crash/restart without data loss
2. **Trust is computed, not assigned** -- per-principal trust evolves from interaction history
3. **Alignment gates action** -- the Align step blocks actions when trust is insufficient
4. **Threads are relationship-aware** -- Threat Monitor flags trust boundary violations; Self-Critique evaluates relationship context
5. **Both engines recover independently (I9)** -- Cognitive AQ and Tool AQ have separate WALs, separate recovery paths
6. **All state is replayable (I3)** -- every relationship signal is stored as an artifact; every tick records the active relationship snapshot

---

## Further Reading

- [Architecture Reference](../architecture.md) -- dual-engine design, PODAARA cycle, relationship substrate
- [Configuration Reference](../configuration.md) -- vessel.toml options, alignment thresholds
- [Data Directory Layout](../data-directory.md) -- persistence structure, crash recovery
- [Acceptance Test Taxonomy](../acceptance-test-taxonomy.md) -- Criterion B (Kill/Restart Durability)
