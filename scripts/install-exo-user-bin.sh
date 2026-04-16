#!/usr/bin/env bash
set -euo pipefail

profile="${1:-release}"

case "$profile" in
  release)
    cargo_args=(build -p exoskeleton-cli --release)
    binary_rel="target/release/exo"
    ;;
  debug)
    cargo_args=(build -p exoskeleton-cli)
    binary_rel="target/debug/exo"
    ;;
  *)
    echo "usage: $0 [release|debug]" >&2
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ -n "${EXO_USER_BIN_DIR:-}" ]]; then
  dest_dir="${EXO_USER_BIN_DIR/#\~/$HOME}"
elif [[ -d "$HOME/bin" ]]; then
  dest_dir="$HOME/bin"
else
  dest_dir="$HOME/.local/bin"
fi

mkdir -p "$dest_dir"

echo "Building exo (${profile})..."
(cd "$repo_root" && cargo "${cargo_args[@]}")

src_bin="$repo_root/$binary_rel"
dest_bin="$dest_dir/exo"

install -m 755 "$src_bin" "$dest_bin"

echo "Installed: $dest_bin"

case ":$PATH:" in
  *":$dest_dir:"*)
    ;;
  *)
    echo "Note: $dest_dir is not currently on PATH." >&2
    ;;
esac
