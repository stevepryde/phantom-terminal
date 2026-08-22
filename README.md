# Phantom Terminal

Phantom Terminal is a minimal desktop terminal that remembers your tabs.
It is a native Rust app: a `winit` window with a hand-rolled `wgpu` renderer for
the terminal grid, `egui` for settings and panels, and `alacritty_terminal` as
the VT core. No webview, no web stack.

![Phantom Terminal preview](docs/assets/phantom-terminal-preview-2026-06-06-1705.webp)

## Features

- It's a terminal app.
- Horizontal or vertical tab layouts, custom tab names, and keyboard shortcuts.
- Theme, font, cursor, shell profile, and session settings.
- Trusted, directory-aware task tabs and local spdeploy actions.
- Scrollback is memory-only, nothing gets written to disk except settings.

## Why

I wanted a simple terminal that remembers tabs and the CWD of each tab.

## Requirements

- A stable Rust toolchain with `cargo`.
- On Linux, the usual `winit` build dependencies (X11/Wayland/xkbcommon client
  headers), e.g. on Debian/Ubuntu:

  ```sh
  sudo apt-get install -y libxkbcommon-dev libwayland-dev libx11-dev \
    libxcursor-dev libxi-dev libxrandr-dev libgl1-mesa-dev
  ```

## Development

Run the app:

```sh
cargo run -p phantom-app
```

Launch a fresh, non-remembering window in a specific directory:

```sh
cargo run -p phantom-app -- --cwd /path/to/project
```

Installed builds accept the same `--cwd` launch mode:

```sh
phantom --cwd /path/to/project
```

`--cwd` launches use your normal settings, but they do not restore remembered
tabs and do not update remembered tab state. Without `--cwd`, Phantom restores
remembered tabs when "Restore tabs on launch" is enabled. Use `--normal` to
force the usual remembered-tabs launch even when another launcher supplies
`--cwd`:

```sh
phantom --normal
```

## Contextual project tasks

Add a `.phantom.yml` to a project directory to describe the tabs you commonly
open there:

```yaml
version: 1
name: Soulfire
tabs:
  - id: api
    title: Soulfire API
    cwd: soulfire/bins/soulfire-api
    run:
      program: cargo
      args: [run]
      env:
        RUST_LOG: info,soulfire=debug
  - id: ui
    title: Soulfire UI
    cwd: soulfire/bins/soulfire-ui
    run:
      program: ./serve.sh
  - id: deploy
    title: Deploy
    cwd: .
```

When the active tab enters that directory, Phantom shows a translucent,
resizable contextual sidebar on the right of the terminal content, below the
full-width titlebar. It reserves terminal text space while the terminal backdrop
continues behind it. The first visit offers an exact task review. Nothing runs
until you choose **Trust project tasks**, and any manifest edit requires approval
again. After approval you can open all declared tabs or one at a time. A tab
without `run` opens the default shell.

Validate a manifest through the same strict parser and canonical cwd checks
without opening Phantom, granting trust, or running a task:

```sh
phantom context validate /path/to/project
```

Phantom also bundles an AI authoring skill that inspects a project, creates or
updates this structured YAML, and runs the read-only validator. Install or
update it for both Codex and Claude with:

```sh
phantom skill install
```

Use `--target codex` or `--target claude` for one agent. Phantom-managed skill
updates are automatic; an unmanaged or locally modified collision is preserved
unless you explicitly rerun with `--force`. The skill cannot trust a manifest
or execute its tasks—new or changed YAML still requires exact review in the
sidebar.

The sidebar stays open across directory changes, including directories with no
project-specific actions. Closing it restores the full terminal width and leaves
a floating reopen icon in the content area's top-right. Each provider section is
a collapsible accordion, and its state—along with sidebar width/open state and
global provider enablement in **Settings → Context Actions**—is remembered across
restarts. Directories stays first in the stable provider order. It combines the
five most recently used and five most frequently used directories, removes
duplicates, and displays them alphabetically in dense rows with the home prefix
shown as `~`; click a row to open that directory in a new tab. Frequent commands
remain visible as a manual reference, but Phantom does not inject commands or
synthetic `cd` lines into a PTY because terminal output cannot authenticate a
shell prompt. The sidebar always remains translucent over the terminal backdrop.
If the current directory contains
`deploy.yml`, the spdeploy provider lists its runnable actions and can open a
selected action in a new tab. Phantom parses the minimal listing fields from
YAML itself, so discovery does not require or invoke the spdeploy CLI. Discovery
is local-only and never executes a project task.

## Build & Install Locally

Phantom Terminal does not include a network auto-updater. To update an installed
copy, pull or switch to the source you want, then build and install that
checkout with one OS-detecting script (no Bun, no Node — just `cargo` and the OS
tools):

```sh
git pull
./scripts/install-native.sh
```

On macOS the source installer also maintains `~/.local/bin/phantom` as a link
to the installed app binary, so the validation and skill commands are available
from a shell. If you copied the `.app` from the DMG instead, invoke the embedded
CLI directly once (or create your own PATH link):

```sh
"/Applications/Phantom Terminal.app/Contents/MacOS/phantom" skill install
```

- **macOS** — assembles `Phantom Terminal.app` around the `phantom` binary, gives
  it the app icon, ad-hoc signs it, and installs it to
  `/Applications/Phantom Terminal.app`. It also writes a drag-to-Applications
  `.dmg` to `target/native-bundle/` for archiving. Ad-hoc signing is all a
  personal, locally-built app needs — no Apple Developer ID and no notarization.
- **Linux** — installs the `phantom` binary, the icon (into the hicolor theme),
  and a `.desktop` entry under `~/.local`. The committed entry is
  [`crates/phantom-app/linux/phantom.desktop`](crates/phantom-app/linux/phantom.desktop).
  Desktop environments that use `xdg-terminal-exec` can select it with the
  desktop id `phantom.desktop`; its `X-TerminalArgDir=--cwd` metadata is used for
  cwd-specific, ephemeral launches.

Useful variants:

```sh
./scripts/install-native.sh --no-install   # build only, leave artifacts in target/
./scripts/install-native.sh --no-dmg       # macOS: skip the .dmg
INSTALL_DIR="$HOME/Applications" ./scripts/install-native.sh   # install elsewhere
```

## Checks

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
bash scripts/check-no-network.sh
```

## License

Phantom Terminal is released under the MIT License. See [LICENSE](LICENSE).
