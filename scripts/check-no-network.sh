#!/usr/bin/env bash
# Enforces Phantom Terminal's "no outbound network" posture across the Rust
# workspace.
set -euo pipefail

repo_root=${PHANTOM_NO_NETWORK_ROOT:-"$(cd "$(dirname "$0")/.." && pwd)"}
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

# Inspect resolved package identities, not manifest dependency keys. Cargo's
# metadata preserves the real package identity when a dependency is renamed.
#
# AccessKit's Unix backend intentionally brings in zbus and async-io to speak
# local D-Bus. They are permitted as transitive implementation details, but are
# still rejected below if a workspace crate starts depending on them directly.
blocked_packages=$(python3 - "$metadata_file" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as metadata_file:
    metadata = json.load(metadata_file)

packages_by_id = {package["id"]: package["name"] for package in metadata["packages"]}
resolved_nodes = metadata["resolve"]["nodes"]
resolved_names = {packages_by_id[node["id"]] for node in resolved_nodes}

# Protocol clients are forbidden anywhere in the resolved graph, including as
# renamed or transitive dependencies.
forbidden_resolved = {
    "attohttpc",
    "awc",
    "curl",
    "fastwebsockets",
    "h2",
    "hyper",
    "hyper-util",
    "isahc",
    "reqwest",
    "surf",
    "tokio-tungstenite",
    "tungstenite",
    "ureq",
    "websocket",
}

# These packages can open sockets without an HTTP/WebSocket client. Some occur
# transitively for platform integration, so reject new direct authority at the
# workspace boundary and pair this with the source scan below.
forbidden_direct = forbidden_resolved | {
    "async-io",
    "async-net",
    "async-std",
    "mio",
    "nng",
    "socket2",
    "tokio",
    "zbus",
    "zmq",
}

blocked = {f"resolved package: {name}" for name in resolved_names & forbidden_resolved}
workspace_ids = set(metadata["workspace_members"])
for node in resolved_nodes:
    if node["id"] not in workspace_ids:
        continue
    for dependency in node["deps"]:
        name = packages_by_id[dependency["pkg"]]
        if name in forbidden_direct:
            blocked.add(f"direct workspace dependency: {name}")

print("\n".join(sorted(blocked)))
PY
)

if [ -n "$blocked_packages" ]; then
  while IFS= read -r blocked_package; do
    printf '  ✗ %s\n' "$blocked_package" >&2
  done <<<"$blocked_packages"
  fail=1
else
  echo "  ✓ none"
fi

echo "==> Scanning workspace Rust sources for direct socket APIs"
# Keep this list to high-signal socket authorities. It catches fully-qualified
# paths, grouped imports after rustfmt, common async/runtime socket modules, and
# imported socket types used through connect/bind.
socket_re='(^|[^[:alnum:]_])((std|tokio|async_std|smol|mio)[[:space:]]*::[[:space:]]*net([^[:alnum:]_]|$)|net[[:space:]]*::[[:space:]]*(TcpStream|TcpListener|UdpSocket)([^[:alnum:]_]|$)|(TcpStream|TcpListener|UdpSocket)[[:space:]]*::[[:space:]]*(connect|bind)([^[:alnum:]_]|$)|socket2[[:space:]]*::|nix[[:space:]]*::[[:space:]]*sys[[:space:]]*::[[:space:]]*socket|libc[[:space:]]*::[[:space:]]*(socket|connect|bind)([^[:alnum:]_]|$))'
socket_matches=$(find crates -type f -name '*.rs' -exec grep -En "$socket_re" {} + 2>/dev/null || true)
if [ -n "$socket_matches" ]; then
  printf '%s\n' "$socket_matches"
  echo "  ✗ A workspace crate uses a direct socket API." >&2
  fail=1
else
  echo "  ✓ none"
fi

if [ "$fail" -ne 0 ]; then
  echo "no-network check FAILED" >&2
  exit 1
fi
echo "no-network check passed"
