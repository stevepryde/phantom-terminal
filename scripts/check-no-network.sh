#!/usr/bin/env bash
# Enforces Phantom Terminal's "no outbound network" posture across the Rust
# workspace.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=${PHANTOM_NO_NETWORK_ROOT:-"$(cd "$script_dir/.." && pwd)"}
cd "$repo_root"

fail=0
metadata_file=$(mktemp "${TMPDIR:-/tmp}/phantom-cargo-metadata.XXXXXX")
trap 'rm -f "$metadata_file"' EXIT

echo "==> Checking the locked Cargo resolve graph for network-capable packages"
if ! command -v python3 >/dev/null 2>&1; then
  echo "  ✗ python3 is required to inspect cargo metadata safely." >&2
  exit 1
fi
if ! cargo metadata --locked --format-version 1 >"$metadata_file"; then
  echo "  ✗ cargo metadata --locked failed." >&2
  exit 1
fi

if ! python3 "$script_dir/check-no-network.py" "$metadata_file"; then
  fail=1
else
  echo "  ✓ none"
fi

if [ "$fail" -ne 0 ]; then
  echo "no-network check FAILED" >&2
  exit 1
fi
echo "no-network check passed"
