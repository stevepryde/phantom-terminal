#!/usr/bin/env bash
# Build the current checkout and install it locally.
#
# This installs WHAT IS CHECKED OUT — it deliberately does not touch git. To
# update first, use your normal workflow (`git pull`, switch branches, etc.) and
# then run this. Keeping the two separate means the script never rewrites itself
# mid-run, and you always know exactly what you're installing.
#
# Phantom Terminal ships no network auto-updater on purpose — the no-network
# security posture (enforced by scripts/check-no-network.sh in CI) forbids the
# tauri-plugin-updater and any outbound HTTP. Rebuilding from source is the
# update path.
#
# Usage:
#   bun run update                  # build release, install over the old copy
#   bun run update -- --no-install  # build only, leave the bundle in target/
#
# Env overrides:
#   INSTALL_DIR=~/Applications bun run update   # install somewhere else (macOS)
set -euo pipefail

note() { printf '\033[1;35m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ── Resolve repo root (this script lives in <root>/scripts) ──────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

INSTALL=1
for arg in "$@"; do
  case "$arg" in
    --no-install) INSTALL=0 ;;
    -h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option: $arg" ;;
  esac
done

PRODUCT_NAME="Phantom Terminal"
VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version" 2>/dev/null || echo '?')"
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

# ── Build the release bundle (tauri runs the frontend build first) ───────────
note "Building $PRODUCT_NAME v$VERSION ($COMMIT)…"
bun run tauri build

if [ "$INSTALL" -eq 0 ]; then
  note "Build complete. Bundle left under src-tauri/target/release/bundle/ (--no-install)."
  exit 0
fi

# ── Install over the existing copy, per-platform ─────────────────────────────
BUNDLE_DIR="src-tauri/target/release/bundle"
OS="$(uname -s)"

case "$OS" in
  Darwin)
    APP_SRC="$BUNDLE_DIR/macos/$PRODUCT_NAME.app"
    [ -d "$APP_SRC" ] || die "expected app bundle not found: $APP_SRC"
    INSTALL_DIR="${INSTALL_DIR:-/Applications}"
    APP_DST="$INSTALL_DIR/$PRODUCT_NAME.app"

    # Quit a running instance so we can replace it cleanly.
    if pgrep -f "$PRODUCT_NAME.app/Contents/MacOS/" >/dev/null 2>&1; then
      note "Quitting running $PRODUCT_NAME…"
      osascript -e "tell application \"$PRODUCT_NAME\" to quit" >/dev/null 2>&1 || true
      sleep 1
      pkill -f "$PRODUCT_NAME.app/Contents/MacOS/" 2>/dev/null || true
    fi

    note "Installing to $APP_DST…"
    mkdir -p "$INSTALL_DIR" 2>/dev/null || true
    if [ ! -d "$INSTALL_DIR" ] || [ ! -w "$INSTALL_DIR" ]; then
      die "$INSTALL_DIR is not writable. Re-run with INSTALL_DIR=\"\$HOME/Applications\" or use sudo."
    fi
    rm -rf "$APP_DST"
    # ditto preserves the bundle's structure and resource forks; cp -R can mangle.
    ditto "$APP_SRC" "$APP_DST"
    # Strip the quarantine flag so a locally-built, ad-hoc-signed app opens
    # without the "unidentified developer" gatekeeper prompt.
    xattr -dr com.apple.quarantine "$APP_DST" 2>/dev/null || true

    note "Done. Launch with: open -a \"$PRODUCT_NAME\""
    ;;

  Linux)
    # Prefer the AppImage (self-contained); fall back to pointing at the .deb.
    appimage="$(ls -t "$BUNDLE_DIR"/appimage/*.AppImage 2>/dev/null | head -n1 || true)"
    if [ -n "$appimage" ]; then
      INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
      mkdir -p "$INSTALL_DIR"
      dst="$INSTALL_DIR/phantom-terminal.AppImage"
      note "Installing AppImage to $dst…"
      install -m 0755 "$appimage" "$dst"
      note "Done. Launch with: $dst"
      case ":$PATH:" in *":$INSTALL_DIR:"*) ;; *) warn "$INSTALL_DIR is not on your PATH.";; esac
    else
      deb="$(ls -t "$BUNDLE_DIR"/deb/*.deb 2>/dev/null | head -n1 || true)"
      [ -n "$deb" ] || die "no AppImage or .deb found under $BUNDLE_DIR"
      note "Built Debian package: $deb"
      note "Install it with: sudo dpkg -i \"$deb\""
    fi
    ;;

  *)
    die "unsupported platform '$OS' — install the bundle from $BUNDLE_DIR manually."
    ;;
esac
