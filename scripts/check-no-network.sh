#!/usr/bin/env bash
# Enforces Phantom Terminal's "no outbound network" posture across the whole
# Rust workspace.
#
# We cannot assert the dependency tree is HTTP-client-free (Tauri links
# reqwest/hyper internally; the native app links no network stack but pulls a
# large GPU/text tree). Instead we assert the things we actually control:
#   1. none of our own crates adds an HTTP-client / websocket dependency,
#   2. no network-exposing Tauri plugin is present,
#   3. window capabilities grant no http / shell / fs access,
#   4. the webview CSP allows no remote origins.
#
# Checks 2–4 are specific to the Tauri build and are skipped automatically once
# src-tauri/ is removed (the native app has no webview boundary at all).
set -euo pipefail

cd "$(dirname "$0")/.."
fail=0

echo "==> Checking workspace crate manifests for HTTP-client dependencies"
http_re='^[[:space:]]*(reqwest|hyper|hyper-util|h2|isahc|ureq|surf|curl|attohttpc|tungstenite|tokio-tungstenite|websocket|awc)[[:space:]]*='
manifests=$(ls crates/*/Cargo.toml 2>/dev/null || true)
[ -f src-tauri/Cargo.toml ] && manifests="$manifests src-tauri/Cargo.toml"
if [ -n "${manifests// /}" ] && grep -nEi "$http_re" $manifests; then
  echo "  ✗ A direct HTTP-client dependency was added to a workspace crate." >&2
  fail=1
else
  echo "  ✓ none"
fi

if [ -d src-tauri ]; then
  echo "==> Checking for network-exposing Tauri plugins in Cargo.lock"
  if grep -nE '^name = "tauri-plugin-(http|updater|websocket)"' Cargo.lock; then
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
fi

if [ "$fail" -ne 0 ]; then
  echo "no-network check FAILED" >&2
  exit 1
fi
echo "no-network check passed"
