#!/bin/sh
# Exoskeleton vessel entrypoint.
#
# Usage:
#   docker compose run -it vessel bootstrap   # First run: interactive wizard
#   docker compose up                         # Subsequent: start vessel
#
# Runs as root to fix /data permissions, then drops to EXO_USER_ID via gosu.

set -e

# ── Resolve runtime user ────────────────────────────────────────
TARGET_UID="${EXO_USER_ID:-1000}"
TARGET_GID="${EXO_GROUP_ID:-1000}"

# Ensure /data is writable by the target user
if [ "$(id -u)" = "0" ]; then
  chown "$TARGET_UID:$TARGET_GID" /data
fi

# ── Route command ────────────────────────────────────────────────
case "${1:-start}" in
  bootstrap)
    shift
    exec gosu "$TARGET_UID:$TARGET_GID" \
      /usr/local/bin/exo bootstrap --data-dir /data "$@"
    ;;

  serve-bootstrap)
    shift
    exec gosu "$TARGET_UID:$TARGET_GID" \
      /usr/local/bin/exo serve-bootstrap \
      --data-dir /data \
      --listen "${EXO_DAEMON_LISTEN:-0.0.0.0:7600}" \
      "$@"
    ;;

  start)
    shift
    if [ ! -f /data/vessel.toml ]; then
      echo ""
      echo "  No vessel.toml found at /data/vessel.toml"
      echo ""
      echo "  Run the bootstrap wizard first:"
      echo "    docker compose run -it vessel bootstrap"
      echo "  Or use serve-bootstrap for programmatic bootstrap:"
      echo "    docker compose run vessel serve-bootstrap"
      echo ""
      exit 1
    fi
    exec gosu "$TARGET_UID:$TARGET_GID" \
      /usr/local/bin/exo start \
      --config /data/vessel.toml \
      --listen "${EXO_DAEMON_LISTEN:-0.0.0.0:7600}" \
      "$@"
    ;;

  *)
    exec gosu "$TARGET_UID:$TARGET_GID" \
      /usr/local/bin/exo "$@"
    ;;
esac
