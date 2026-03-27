# Schema Migration Strategy

How persistent stores evolve across Exoskeleton versions.

---

## Problem

Exoskeleton uses 8 SQLite stores, each created by Rust code with hardcoded `CREATE TABLE` statements. WorldInterface has 1 SQLite store. Observatory has 1 SQLite store. ActionQueue uses a custom WAL + snapshot format. There is currently no mechanism to evolve these schemas without data loss.

Before production deployment, every store needs a migration story.

---

## Store Inventory

| Store | Location | Owner | Format |
|-------|----------|-------|--------|
| ArtifactStore | `{data_dir}/artifacts.db` | exoskeleton-host | SQLite |
| SnapshotStore | `{data_dir}/snapshots.db` | exoskeleton-host | SQLite |
| EventLedger | `{data_dir}/events.db` | exoskeleton-host | SQLite |
| TickStore | `{data_dir}/ticks.db` | exoskeleton-host | SQLite |
| MemoryStore | `{data_dir}/memory.db` | exoskeleton-host | SQLite |
| ThreadStore | `{data_dir}/threads.db` | exoskeleton-host | SQLite |
| RelationshipLedger | `{data_dir}/relationships.db` | exoskeleton-host | SQLite |
| BudgetStore | `{data_dir}/budgets.db` | exoskeleton-host | SQLite |
| WatchStore | `{data_dir}/watches.db` | exoskeleton-host | SQLite |
| WI ContextStore | `{data_dir}/wi/context.db` | worldinterface-contextstore | SQLite |
| Fleet Store | `observatory.db` | observatory-server | SQLite |
| Auth Store | `observatory.db` | observatory-server | SQLite |
| AQ WAL | `{data_dir}/cognitive-aq/` | actionqueue-storage | Postcard binary v5 |
| AQ Snapshots | `{data_dir}/cognitive-aq/` | actionqueue-storage | Schema v8 |

---

## Approach: Version Table + SQL Migrations

Each SQLite database gets a `_schema_version` table:

```sql
CREATE TABLE IF NOT EXISTS _schema_version (
    version INTEGER NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    description TEXT
);
```

On startup, each store:
1. Creates `_schema_version` if it doesn't exist (version 0 = initial schema)
2. Reads current version
3. Applies all migrations from current+1 to target version
4. Updates `_schema_version`

### Migration Format

Migrations are embedded in the Rust binary as SQL strings:

```rust
const MIGRATIONS: &[(u32, &str, &str)] = &[
    // (version, description, sql)
    (1, "Initial schema", "CREATE TABLE IF NOT EXISTS ..."),
    (2, "Add watch_store table", "CREATE TABLE IF NOT EXISTS watches (...)"),
    (3, "Add trust_history index", "CREATE INDEX IF NOT EXISTS ..."),
];
```

### Implementation

Each store module (e.g., `SqliteTickStore`) gains:

```rust
impl SqliteTickStore {
    pub fn open(path: &Path) -> Result<Self, ExoError> {
        let conn = Connection::open(path)?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    fn migrate(conn: &Connection) -> Result<(), ExoError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _schema_version (
                version INTEGER NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now')),
                description TEXT
            )"
        )?;

        let current: u32 = conn
            .query_row("SELECT COALESCE(MAX(version), 0) FROM _schema_version", [], |r| r.get(0))
            .unwrap_or(0);

        for &(version, description, sql) in MIGRATIONS {
            if version > current {
                conn.execute_batch(sql)?;
                conn.execute(
                    "INSERT INTO _schema_version (version, description) VALUES (?1, ?2)",
                    rusqlite::params![version, description],
                )?;
            }
        }
        Ok(())
    }
}
```

---

## ActionQueue WAL/Snapshot Versioning

ActionQueue already handles this internally:
- WAL format has a version byte (currently v5)
- Snapshot format has a schema version (currently v8)
- Recovery code validates versions and rejects incompatible formats

No additional migration mechanism needed for AQ.

---

## WI ContextStore

The ContextStore is write-once per `(FlowRunId, NodeId)`. Schema changes are rare. The same version-table approach applies, but migrations will be infrequent.

---

## Observatory Stores

Fleet Store and Auth Store share `observatory.db`. Same version-table approach, with separate version tracking per logical store (e.g., `_fleet_schema_version`, `_auth_schema_version`).

---

## Implementation Timeline

This should be implemented in Epoch 6 (Scale & Production), Sprint 1:
1. Add `_schema_version` pattern to `StorageManager` in exoskeleton-host
2. Retrofit existing `CREATE TABLE` statements as migration v1
3. Apply the same pattern to Observatory stores
4. Document the migration authoring process

New features that change schemas must include a migration entry.
