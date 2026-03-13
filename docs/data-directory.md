# Data Directory Layout

All persistent state for an Exoskeleton Vessel lives under a single `data_dir` root. The directory structure reflects the dual-engine architecture (I9): cognitive and tool engines have physically separate directories with independent WALs.

---

## Full Directory Tree

```
{data_dir}/
├── cognitive-aq/              # Cognitive AQ engine (owned by exoskeleton-host)
│   ├── wal                    # Append-only WAL (postcard binary + CRC-32)
│   └── snapshot               # Derived acceleration snapshot (atomic writes)
│
├── wi/                        # WorldInterface Host (owns Tool AQ)
│   ├── aq/                    # Tool AQ engine (owned by WI Host)
│   │   ├── wal                # Append-only WAL (independent of cognitive-aq)
│   │   └── snapshot           # Derived acceleration snapshot
│   └── context.db             # WI context store (SQLite)
│
├── exo/                       # Exoskeleton application stores (8 SQLite databases)
│   ├── artifacts.db           # Content-addressed artifact store
│   ├── snapshots.db           # State snapshot history
│   ├── events.db              # Append-only event ledger
│   ├── ticks.db               # Completed tick records
│   ├── memory.db              # Episodic summaries + long-term notes
│   ├── threads.db             # Thread specs + outputs
│   ├── relationships.db       # Append-only relationship ledger
│   └── budget.db              # Budget window state
│
└── inbox/                     # File-based message inbox (default location)
    └── *.json                 # MessageEnvelope files (consumed by Perceive)
```

---

## cognitive-aq/

The Cognitive AQ directory contains the WAL and snapshot for the cognitive engine. This engine runs:

- Master loop tasks (PODAARA ticks)
- Thread execution dispatch
- LLM inference routing

**Files:**

| File | Format | Description |
|------|--------|-------------|
| `wal` | Binary (postcard + CRC-32) | Append-only write-ahead log of all cognitive task state transitions |
| `snapshot` | Binary | Acceleration snapshot derived from WAL; atomic writes via temp+rename |

The WAL is the authoritative source of truth for the Cognitive AQ. The snapshot is a derived artifact that accelerates recovery. On startup, the engine reconstructs state from the snapshot + WAL tail, or from the WAL alone if the snapshot is missing or corrupt.

---

## wi/

The WorldInterface Host directory contains the Tool AQ engine and WI context store. Exoskeleton treats this as a black box -- it never reads or writes these files directly.

### wi/aq/

| File | Format | Description |
|------|--------|-------------|
| `wal` | Binary (postcard + CRC-32) | Append-only WAL for tool task state transitions |
| `snapshot` | Binary | Tool AQ acceleration snapshot |

### wi/context.db

| File | Format | Description |
|------|--------|-------------|
| `context.db` | SQLite | WI context store for connector state, workflow metadata |

The Tool AQ has its own WAL, independent of the Cognitive AQ (I9). The two WALs are never merged and recover independently.

---

## exo/

The Exoskeleton application stores contain all domain-level state. Each store is a separate SQLite database file with WAL journal mode enabled.

### artifacts.db

**Content-addressed artifact store.** Every meaningful piece of data produced by the system is stored here with a SHA-256 content address (`ArtifactId`).

Artifact kinds include: `Snapshot`, `Plan`, `LlmResponse`, `Receipt`, `ThreadOutput`, `RelationshipSnapshot`.

Used for I3 (everything replayable). The Amend step stores snapshot artifacts; the Decide step stores LLM response artifacts; threads store ThreadOutput artifacts; the Act step stores execution receipts.

### snapshots.db

**State snapshot history.** Each PODAARA tick produces a `StateSnapshot` that is saved here. Contains the current tick number, vessel status, pending actions, thread summaries, budget status, and relationship snapshot reference.

The `latest()` query drives the master loop -- each tick begins by loading the most recent snapshot.

### events.db

**Append-only event ledger.** Records significant lifecycle events: `VesselStarted`, `TickStarted`, `TickCompleted`, `ActionExecuted`, `RelationshipUpdated`, `Error`, and others.

Each entry has a `LedgerEntryId`, optional `TickId`, `EventType`, optional `payload_ref` (artifact reference), and human-readable summary.

### ticks.db

**Completed tick records.** Each tick produces a `TickRecord` with: tick_id, tick_number, started_at, completed_at, snapshot_before (artifact reference), thread_contributions, llm_calls, action_records, and perception metadata.

Supports range queries for thrash detection (`range(start_tick, end_tick)`) and the tick history API endpoint.

### memory.db

**Memory tiers.** Contains two tables:

- **Episodic summaries** -- compressed narratives spanning a range of ticks, with key events and token counts
- **Long-term notes** -- persistent topic-keyed knowledge extracted by the Memory Consolidation thread

The Context Compiler reads from both tables during Orient to assemble the context window (I5: compiled, not accumulated).

### threads.db

**Thread specifications and outputs.** Stores `ThreadSpec` records (name, charter, priority, token_budget, schedule) and their execution status.

Built-in threads (Threat Monitor, Self-Critique, Memory Consolidation) are registered at boot with deterministic UUIDs. Custom threads can be registered via the thread registry.

### relationships.db

**Append-only relationship ledger.** Every relational signal is recorded here: trust updates, commitment fulfillment/breakage, feedback, alignment mismatches.

Each record references a principal, signal type, content artifact, and tick. The `compile_relationship_snapshot()` function reads the full ledger and produces per-principal trust scores and summaries.

### budget.db

**Budget window state.** Persists the current budget window's consumption counters across restarts. Contains local tokens consumed, frontier tokens consumed, frontier cost, frontier calls, per-thread consumption, consecutive failures, and tool invocations.

On startup, the `CognitiveBudgetTracker` loads from this store (or resets if the window has expired). On each tick, updated state is persisted.

---

## inbox/

The file-based inbox directory (default: `{data_dir}/inbox/`). Message envelopes are written as JSON files and consumed by the Perceive step.

Each file is a `MessageEnvelope` with: envelope_id, source (PrincipalId), target, kind (HumanMessage, etc.), payload_ref (ArtifactId), timestamp, and optional in_reply_to.

The inbox directory can be overridden via `vessel.inbox_dir` or `EXO_DATA_DIR`.

---

## File Lifecycle

### Startup

1. `ensure_data_dirs()` creates all required subdirectories under `data_dir`
2. `StorageManager::open()` opens or creates all 8 SQLite databases under `exo/`
3. Cognitive AQ bootstraps from `cognitive-aq/` (WAL + snapshot recovery)
4. WI Host bootstraps from `wi/` (Tool AQ WAL + snapshot recovery, context store)

### Steady State

- **cognitive-aq/wal** grows with each cognitive task state transition
- **wi/aq/wal** grows with each tool invocation state transition
- **exo/*.db** grow with each tick (artifacts, snapshots, events, ticks, etc.)
- **inbox/*.json** files are consumed and removed by Perceive

### Crash Recovery

Both engines recover independently (I9):

1. Cognitive AQ reconstructs from `cognitive-aq/snapshot` + `cognitive-aq/wal` tail
2. Tool AQ reconstructs from `wi/aq/snapshot` + `wi/aq/wal` tail
3. SQLite databases are self-consistent (WAL journal mode provides crash safety)
4. Budget state is loaded from `budget.db` (or reset if window expired)
5. Tick numbers resume monotonically from the latest tick in `ticks.db`

### Backup

To back up a running Vessel:

1. Copy the `exo/` directory (SQLite WAL mode ensures consistent reads)
2. Copy `cognitive-aq/` and `wi/aq/` directories (snapshot + WAL pairs)
3. Copy `inbox/` if message preservation is needed

Individual SQLite databases can be backed up independently since they are separate files with no cross-database dependencies.
