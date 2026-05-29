#!/usr/bin/env bash
# Painless local "auto-update" for Phantom Terminal.
#
# Phantom Terminal ships no network auto-updater on purpose — the no-network
# security posture (enforced by scripts/check-no-network.sh in CI) forbids the
# tauri-plugin-updater and any outbound HTTP. So "updating" means: pull the
# latest source, build a fresh release bundle, and install it over the old one.
#
# Usage:
#   bun run update           # pull latest, build release, install
#   bun run update -- --no-pull   # build the working tree as-is, then install
#   bun run update -- --no-install # build only, leave the bundle in target/
#
# Env overrides:
#   INSTALL_DIR=~/Applications bun run update   # install somewhere else (macOS)
set -euo pipefail

# ── Resolve repo root (this script lives in <root>/scripts) ──────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

PULL=1
INSTALL=1
for arg in "$@"; do
  case "$arg" in
    --no-pull) PULL=0 ;;
    --no-install) INSTALL=0 ;;
    -h|--help)
      sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

note() { printf '\033[1;35m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

PRODUCT_NAME="Phantom Terminal"
OLD_VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version" 2>/dev/null || echo '?')"

# ── 1. Pull latest source (unless --no-pull or the tree is dirty) ────────────
if [ "$PULL" -eq 1 ]; then
  if [ -n "$(git status --porcelain)" ]; then
    warn "working tree has uncommitted changes — skipping git pull, building as-is."
  else
    branch="$(git rev-parse --abbrev-ref HEAD)"
    note "Pulling latest on '$branch' (fast-forward only)…"
    if git pull --ff-only; then :; else
      warn "fast-forward pull failed (diverged branch?) — building current commit."
    fi
  fi
fi

NEW_VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version" 2>/dev/null || echo '?')"
note "Building $PRODUCT_NAME v$NEW_VERSION (was v$OLD_VERSION)…"

# ── 2. Build the release bundle (tauri runs the frontend build first) ────────
# --no-bundle would skip packaging; we want the installable .app/.deb/.AppImage.
bun run tauri build

if [ "$INSTALL" -eq 0 ]; then
  note "Build complete. Bundle left under src-tauri/target/release/bundle/ (--no-install)."
  exit 0
fi

# ── 3. Install over the existing copy, per-platform ──────────────────────────
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
