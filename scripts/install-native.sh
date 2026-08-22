#!/usr/bin/env bash
# Build the native Phantom Terminal app (crates/phantom-app) and install it
# locally. No Bun, no Node, no network - just cargo and the OS tools.
#
# It installs WHAT IS CHECKED OUT and never touches git: update with your normal
# workflow (`git pull`, switch branches) first, then run this. Phantom ships no
# network auto-updater on purpose - rebuilding from source is the update path.
#
# macOS:
#   - Assembles a "Phantom Terminal.app" bundle around the `phantom` binary,
#     using the app icon at assets/icons/icon.icns.
#   - Ad-hoc signs it (codesign -s -). That is all a personal, locally-built app
#     needs to run - no Apple Developer ID and no notarization required. Those
#     only matter for distributing to *other* machines, where Gatekeeper checks
#     a Developer ID signature + notarization ticket. For your own machine an
#     ad-hoc signature plus stripping the quarantine flag is enough.
#   - Installs to /Applications/Phantom Terminal.app (override with INSTALL_DIR).
#   - Links the bundled CLI to ~/.local/bin/phantom (override with
#     CLI_INSTALL_DIR) when that path is free or already Phantom-owned.
#   - Also writes a drag-to-Applications .dmg to target/native-bundle/ (--no-dmg
#     to skip). Built with the built-in hdiutil - no network, no extra tooling.
#
# Linux:
#   - Installs the `phantom` binary, the icon, and a .desktop entry under
#     ~/.local (override the binary dir with INSTALL_DIR).
#
# Usage:
#   ./scripts/install-native.sh                  # build release, install
#   ./scripts/install-native.sh --no-install     # build only, leave in target/
#   ./scripts/install-native.sh --no-dmg         # macOS: skip the .dmg artifact
#
# Env overrides:
#   INSTALL_DIR=~/Applications ./scripts/install-native.sh   # install elsewhere
set -euo pipefail

note() { printf '\033[1;35m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Resolve repo root (this script lives in <root>/scripts).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

INSTALL=1
DMG=1
for arg in "$@"; do
  case "$arg" in
    --no-install) INSTALL=0 ;;
    --no-dmg) DMG=0 ;;
    -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option: $arg" ;;
  esac
done

PRODUCT_NAME="Phantom Terminal"
BIN_NAME="phantom"
IDENTIFIER="com.phantom.terminal"
# Version is read from the native crate's manifest (the source of truth here).
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/phantom-app/Cargo.toml | head -n1)"
VERSION="${VERSION:-0.0.0}"
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

note "Building ${PRODUCT_NAME} v${VERSION} (${COMMIT}) [native]..."
cargo build --release -p phantom-app

BIN_SRC="target/release/$BIN_NAME"
[ -x "$BIN_SRC" ] || die "expected binary not found: $BIN_SRC"

OS="$(uname -s)"

# Cargo requests symbol stripping through the release profile, but some Rust
# toolchains leave the binary untouched when their optional rust-objcopy cannot
# load. The native strip tool makes the install artifact deterministic.
if command -v strip >/dev/null 2>&1; then
  case "$OS" in
    Darwin) strip "$BIN_SRC" ;;
    Linux) strip --strip-all "$BIN_SRC" ;;
  esac
else
  warn "native strip tool unavailable; relying on Cargo release-profile stripping."
fi

case "$OS" in
  Darwin)
    # Assemble a .app bundle around the binary by hand (cargo produces a plain
    # executable, not a macOS bundle).
    APP_STAGE="target/native-bundle/$PRODUCT_NAME.app"
    note "Assembling ${PRODUCT_NAME}.app..."
    rm -rf "$APP_STAGE"
    mkdir -p "$APP_STAGE/Contents/MacOS" "$APP_STAGE/Contents/Resources"
    cp "$BIN_SRC" "$APP_STAGE/Contents/MacOS/$BIN_NAME"
    cp assets/icons/icon.icns "$APP_STAGE/Contents/Resources/icon.icns"
    cat > "$APP_STAGE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>$PRODUCT_NAME</string>
	<key>CFBundleDisplayName</key>
	<string>$PRODUCT_NAME</string>
	<key>CFBundleExecutable</key>
	<string>$BIN_NAME</string>
	<key>CFBundleIdentifier</key>
	<string>$IDENTIFIER</string>
	<key>CFBundleIconFile</key>
	<string>icon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>10.13</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
PLIST

    # Ad-hoc sign the finished bundle so it has a stable identity on this machine.
    note "Ad-hoc signing..."
    codesign --force --sign - --timestamp=none "$APP_STAGE" >/dev/null

    # Produce a .dmg artifact (drag-to-Applications layout) for archiving or
    # copying to another Mac. Pure built-in tooling (hdiutil), no network.
    if [ "$DMG" -eq 1 ]; then
      DMG_PATH="target/native-bundle/$PRODUCT_NAME-$VERSION.dmg"
      DMG_STAGE="target/native-bundle/dmg"
      note "Building ${DMG_PATH##*/}..."
      rm -rf "$DMG_STAGE" "$DMG_PATH"
      mkdir -p "$DMG_STAGE"
      ditto "$APP_STAGE" "$DMG_STAGE/$PRODUCT_NAME.app"
      ln -s /Applications "$DMG_STAGE/Applications"
      hdiutil create -volname "$PRODUCT_NAME" -srcfolder "$DMG_STAGE" \
        -ov -format UDZO "$DMG_PATH" >/dev/null
      rm -rf "$DMG_STAGE"
      note "Disk image: $DMG_PATH"
    fi

    if [ "$INSTALL" -eq 0 ]; then
      note "Build complete. Bundle left at $APP_STAGE (--no-install)."
      exit 0
    fi

    INSTALL_DIR="${INSTALL_DIR:-/Applications}"
    APP_DST="$INSTALL_DIR/$PRODUCT_NAME.app"

    # Quit a running instance so we can replace it cleanly.
    if pgrep -f "$PRODUCT_NAME.app/Contents/MacOS/" >/dev/null 2>&1; then
      note "Quitting running ${PRODUCT_NAME}..."
      osascript -e "tell application \"$PRODUCT_NAME\" to quit" >/dev/null 2>&1 || true
      sleep 1
      pkill -f "$PRODUCT_NAME.app/Contents/MacOS/" 2>/dev/null || true
    fi

    note "Installing to ${APP_DST}..."
    mkdir -p "$INSTALL_DIR" 2>/dev/null || true
    if [ ! -d "$INSTALL_DIR" ] || [ ! -w "$INSTALL_DIR" ]; then
      die "$INSTALL_DIR is not writable. Re-run with INSTALL_DIR=\"\$HOME/Applications\" or use sudo."
    fi
    rm -rf "$APP_DST"
    # ditto preserves the bundle structure and the code signature; cp -R can mangle it.
    ditto "$APP_STAGE" "$APP_DST"
    # Locally built so there is normally no quarantine flag, but strip it anyway
    # so the app never trips the "unidentified developer" prompt.
    xattr -dr com.apple.quarantine "$APP_DST" 2>/dev/null || true

    CLI_INSTALL_DIR="${CLI_INSTALL_DIR:-$HOME/.local/bin}"
    cli_dst="$CLI_INSTALL_DIR/$BIN_NAME"
    cli_target="$APP_DST/Contents/MacOS/$BIN_NAME"
    mkdir -p "$CLI_INSTALL_DIR"
    if [ -L "$cli_dst" ]; then
      existing_target="$(readlink "$cli_dst")"
      case "$existing_target" in
        *"/$PRODUCT_NAME.app/Contents/MacOS/$BIN_NAME")
          ln -sfn "$cli_target" "$cli_dst"
          note "Updated CLI link: $cli_dst"
          ;;
        *) warn "$cli_dst is an unrelated symlink; leaving it unchanged." ;;
      esac
    elif [ -e "$cli_dst" ]; then
      warn "$cli_dst already exists and is not a Phantom app link; leaving it unchanged."
    else
      ln -s "$cli_target" "$cli_dst"
      note "Installed CLI link: $cli_dst"
    fi

    note "Done. Launch with: open -a \"$PRODUCT_NAME\""
    case ":$PATH:" in *":$CLI_INSTALL_DIR:"*) ;; *) warn "$CLI_INSTALL_DIR is not on your PATH.";; esac
    ;;

  Linux)
    if [ "$INSTALL" -eq 0 ]; then
      note "Build complete. Binary left at $BIN_SRC (--no-install)."
      exit 0
    fi

    INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
    mkdir -p "$INSTALL_DIR"
    bin_dst="$INSTALL_DIR/$BIN_NAME"
    note "Installing binary to ${bin_dst}..."
    install -m 0755 "$BIN_SRC" "$bin_dst"

    data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
    applications_dir="$data_dir/applications"
    mkdir -p "$applications_dir"

    # Install the icon at every size we ship, into the hicolor theme.
    declare -A icons=(
      [32x32]="assets/icons/32x32.png"
      [64x64]="assets/icons/64x64.png"
      [128x128]="assets/icons/128x128.png"
      [512x512]="assets/icons/icon.png"
    )
    for size in "${!icons[@]}"; do
      src="${icons[$size]}"
      [ -f "$src" ] || continue
      icon_dir="$data_dir/icons/hicolor/$size/apps"
      mkdir -p "$icon_dir"
      install -m 0644 "$src" "$icon_dir/$BIN_NAME.png"
    done

    # Install the .desktop entry, pointing Exec at the binary we just installed.
    desktop_dst="$applications_dir/$BIN_NAME.desktop"
    note "Installing desktop entry to ${desktop_dst}..."
    sed "s|^Exec=.*|Exec=$bin_dst|" \
      crates/phantom-app/linux/phantom.desktop > "$desktop_dst"
    chmod 0644 "$desktop_dst"

    if command -v update-desktop-database >/dev/null 2>&1; then
      update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
      gtk-update-icon-cache -f -t "$data_dir/icons/hicolor" >/dev/null 2>&1 || true
    fi

    note "Done. Launch from your app menu, or run: $bin_dst"
    case ":$PATH:" in *":$INSTALL_DIR:"*) ;; *) warn "$INSTALL_DIR is not on your PATH.";; esac
    ;;

  *)
    die "unsupported platform '$OS' - the binary is at $BIN_SRC; install it manually."
    ;;
esac
