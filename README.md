# phantom-terminal

Terminal app with some features I want. Built with Tauri 2 (Rust) + React + Vite,
managed with [Bun](https://bun.sh).

## Prerequisites

- [Bun](https://bun.sh) (`1.3.9`, matching CI)
- A stable Rust toolchain (`rustup` + `cargo`)
- Tauri's platform build deps — see the [Tauri prerequisites](https://tauri.app/start/prerequisites/).
  On macOS that's just the Xcode command-line tools.

```sh
bun install
```

## Develop

```sh
bun run tauri dev      # hot-reloading desktop app
```

## Update your local install

There is **no network auto-updater** — by design. Phantom Terminal enforces a
no-outbound-network posture (see `scripts/check-no-network.sh`, gated in CI), so
`tauri-plugin-updater` and any HTTP client are forbidden. Instead you "update"
by rebuilding from source. Pull (or switch branches) however you normally would,
then build and install the current checkout with a single command:

```sh
git pull          # your normal git workflow — optional
bun run update    # build the current checkout and install it
```

This builds a release bundle from **what is checked out** and installs it over
the previous copy — into `/Applications` on macOS, or `~/.local/bin` (AppImage)
on Linux. It does not touch git, so you always know exactly what you're
installing.

```sh
bun run update -- --no-install   # build only; leave the bundle in target/
INSTALL_DIR="$HOME/Applications" bun run update   # install elsewhere (macOS)
```

The macOS app is ad-hoc signed locally, so the script also strips the
quarantine flag to avoid the "unidentified developer" prompt.

## Checks (what CI runs)

```sh
bun run typecheck   # tsc --noEmit
bun run lint        # Biome
bun run build       # tsc + vite production build
bun run audit       # bun audit (high/critical)
(cd src-tauri && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings)
(cd src-tauri && cargo build --locked && cargo test --locked)
bash scripts/check-no-network.sh
```
