#!/usr/bin/env bash
# Enforces Phantom Terminal's "no outbound network" posture across the Rust
# workspace.
#
# We cannot assert the whole dependency tree is HTTP-client-free (the native app
# links no network stack but pulls a large GPU/text tree). Instead we assert the
# thing we actually control: none of our own crates adds an HTTP-client /
# websocket dependency. The native app has no webview, CSP, or capability surface
# to police — removing those is exactly why the check is now this short.
set -euo pipefail

cd "$(dirname "$0")/.."
fail=0

echo "==> Checking workspace crate manifests for HTTP-client dependencies"
http_re='^[[:space:]]*(reqwest|hyper|hyper-util|h2|isahc|ureq|surf|curl|attohttpc|tungstenite|tokio-tungstenite|websocket|awc)[[:space:]]*='
manifests=$(ls crates/*/Cargo.toml 2>/dev/null || true)
if [ -n "${manifests// /}" ] && grep -nEi "$http_re" $manifests; then
  echo "  ✗ A direct HTTP-client dependency was added to a workspace crate." >&2
  fail=1
else
  echo "  ✓ none"
fi

if [ "$fail" -ne 0 ]; then
  echo "no-network check FAILED" >&2
  exit 1
fi
echo "no-network check passed"
