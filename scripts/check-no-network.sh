#!/usr/bin/env bash
# Enforces Phantom Terminal's "no outbound network" posture.
#
# Tauri's core links reqwest/hyper internally, so we cannot assert the dependency
# tree is HTTP-client-free. Instead we assert the things we actually control:
#   1. our own crate adds no HTTP-client dependency,
#   2. no network-exposing Tauri plugin is present,
#   3. window capabilities grant no http / shell / fs access.
set -euo pipefail

cd "$(dirname "$0")/.."
fail=0

echo "==> Checking src-tauri/Cargo.toml for HTTP-client dependencies"
if grep -nEi '^[[:space:]]*(reqwest|hyper|isahc|ureq|surf|curl|attohttpc|tungstenite|tokio-tungstenite|websocket)[[:space:]]*=' src-tauri/Cargo.toml; then
  echo "  ✗ A direct HTTP-client dependency was added to the backend." >&2
  fail=1
else
  echo "  ✓ none"
fi

echo "==> Checking for network-exposing Tauri plugins in Cargo.lock"
if grep -nE '^name = "tauri-plugin-(http|updater|websocket)"' src-tauri/Cargo.lock; then
  echo "  ✗ A network-exposing Tauri plugin is present." >&2
  fail=1
else
  echo "  ✓ none"
fi

echo "==> Checking window capabilities for http/shell/fs permissions"
if grep -rnE '"(http|shell|fs|dialog|upload):' src-tauri/capabilities; then
  echo "  ✗ A capability grants http/shell/fs/dialog access to the webview." >&2
  fail=1
else
  echo "  ✓ none"
fi

echo "==> Checking CSP forbids remote connect-src"
if grep -qE '"csp"[[:space:]]*:[[:space:]]*null' src-tauri/tauri.conf.json; then
  echo "  ✗ CSP is null (disabled)." >&2
  fail=1
else
  # Extract the csp string, drop the one allowed local IPC origin, then look for
  # any remaining remote http(s):// origin.
  csp=$(grep -oE '"csp"[[:space:]]*:[[:space:]]*"[^"]*"' src-tauri/tauri.conf.json | sed 's#http://ipc.localhost##g')
  if printf '%s' "$csp" | grep -qiE 'https?://'; then
    echo "  ✗ CSP allows a remote http(s) origin." >&2
    fail=1
  else
    echo "  ✓ CSP has no remote origins"
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "no-network check FAILED" >&2
  exit 1
fi
echo "no-network check passed"
