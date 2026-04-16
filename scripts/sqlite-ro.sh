#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <db-path> [sql]" >&2
  echo "Example: $0 /path/to/snapshots.db 'select count(*) from snapshots;'" >&2
  exit 1
fi

db_path="$1"
shift

if [[ ! -f "$db_path" ]]; then
  echo "Database file not found: $db_path" >&2
  exit 1
fi

# Open SQLite databases in immutable read-only mode so inspection works even
# when the DB lives outside writable roots or the caller cannot create lock
# side-files next to it.
sqlite3 "file:${db_path}?mode=ro&immutable=1" "$@"
