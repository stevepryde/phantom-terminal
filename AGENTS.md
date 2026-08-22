# AGENTS.md — Phantom Terminal

Guidance for any AI agent (or human) working in this repository. Read this
before making changes.

Phantom Terminal is a Warp-like terminal: a **native Rust desktop app**. A
`winit` window hosts a hand-rolled **wgpu** renderer (`phantom-gfx`) for the
terminal grid, with **egui** drawing the non-terminal control surfaces (settings,
command palette, contextual panels). The VT core is `alacritty_terminal` wrapped
by `phantom-emu`; PTYs, the SQLite session store, and all config validation live
in the UI-agnostic `phantom-core`. There is no webview, no IPC boundary, and no
JavaScript — the whole app is one Rust process.

## Priority order (non-negotiable)

When priorities conflict, resolve them in this order. This ordering governs
design decisions, review focus, and what gets fixed first.

1. **Security** — this app runs shells and handles secrets (env vars, pasted
   tokens, scrollback). Never weaken the no-network posture, the supply-chain
   gates, the at-rest data protections, or backend input validation to make a
   feature easier. A feature that cannot be built securely does not ship.
2. **Correctness** — behavior must match what the user and the code claim.
   Prefer a smaller feature that is fully correct over a larger one that is
   subtly wrong. No half-wired features that pretend to work.
3. **Maintainability & testability** — clear module boundaries, small units,
   tests for logic, no brittle coupling to library internals. Code is read far
   more than it is written.

Performance matters but never overrides the three above. Do not trade away
security or correctness for speed.

## The security model (do not regress)

This is the project's defining characteristic. Preserve every item here.

- **No outbound network, by design.** No HTTP client, no auto-updater, no
  telemetry. Enforced during required local validation by
  `scripts/check-no-network.sh`, which fails if any workspace crate adds a
  network-capable (HTTP-client/websocket) dependency. Updates happen by
  rebuilding from source (`scripts/install-native.sh`).
- **Execution authorities are stored and typed.** Normal tabs resolve
  command/args from a **stored, validated `ShellProfile`** by id (see
  `phantom-core/src/spawn.rs` and `config.rs`). Contextual project tasks are the
  single approved extension: `.phantom.yml` is inert until its exact bounded
  source and canonical root are explicitly trusted and stored as validated,
  structured program/args/env task records. A changed source invalidates trust.
  Built-in contextual providers may launch only a provider-owned fixed
  executable/flag shape after an explicit user action. The only synthesized
  PTY input is a user-selected, validated frequent command or recent-directory
  `cd` line; both require a shell-like prompt, non-application terminal modes,
  and the spawned shell owning the kernel's PTY foreground process group.
  Config is read from disk, which a local attacker could tamper with, so treat
  any change to profile/task validation or spawn resolution as
  security-sensitive and keep validation exhaustive.
- **All config is validated in Rust before use or storage.**
  `AppConfig::validate()` in `phantom-core/src/config.rs` bounds every field
  (lengths, ranges, enums, hex colors, NUL bytes, profile/keybinding counts).
  Any new config field MUST get validation in the same pass — even a draft value
  edited inside an egui widget must round-trip through `validate()` before it is
  committed or persisted.
- **Session data is protected at rest.** The SQLite store
  (`phantom-core/src/session.rs`) is created `0600` in a `0700` dir, with
  `secure_delete=ON`. Only tab title / cwd / order is persisted. Terminal
  scrollback is in-memory only and is never written to disk. Keep it that way.
- **Supply chain is gated locally.** Keep `Cargo.lock` frozen and run
  `cargo-audit`, `cargo-deny`, `osv-scanner`, and SBOM generation from a trusted
  local environment when applicable. The advisory ignore lists in `deny.toml`
  and `osv-scanner.toml` are currently empty and must stay in sync; only ever
  add an *unmaintained/unsound* informational advisory with a written
  justification, never a real vulnerability. Strongly prefer Rust and
  dependency-free solutions over adding a crate.

If you believe a security control should change, stop and ask the user first.

## Automation policy: no CI or GitHub Actions

- This repository intentionally has no hosted continuous-integration service.
- Do not add GitHub Actions workflows or any files under `.github/workflows/`.
- Do not add another hosted CI provider as a substitute.
- Required checks are run locally before changes are declared complete or
  published. Report every check that was actually run and any local tooling
  that was unavailable.
- GitHub may host the repository, issues, and pull requests, but must not execute
  repository code through Actions.

## Repository layout

```
crates/
  phantom-core/   UI-agnostic backend (no GUI deps)
    config.rs       AppConfig + exhaustive validation — the validation trust point
    session.rs      SQLite session/config store (0600 file in 0700 dir, secure_delete)
    pty.rs          PTY lifecycle, shell env, cwd lookup
    spawn.rs        ShellProfile → command/args resolution (no arbitrary commands)
    launch.rs       launch-mode parsing (--cwd / --normal)
    error.rs        AppError / AppResult
  phantom-emu/    VT emulation over alacritty_terminal (grid + snapshot)
  phantom-gfx/    hand-rolled wgpu renderer: glyphs, cells, cursor, backdrop, scrollbar
    assets/backgrounds/   terminal backdrop images, compiled in via include_bytes!
  phantom-app/    the `phantom` binary: winit window, event loop, egui UI
    lib.rs          app state + event loop (run())
    chrome.rs       custom window chrome (titlebar, traffic lights / Linux controls)
    egui_ui.rs      settings / panels / command palette (egui)
    input.rs, keybindings.rs, palette.rs, tab.rs, themes.rs, gpu.rs, blur.rs
assets/icons/     app icon, used by scripts/install-native.sh to build the bundle/install
scripts/
  install-native.sh   build + install locally (macOS .app + .dmg, Linux binary + .desktop)
  check-no-network.sh no-network posture check
ui-design-language.md      the UI spec — read before any UI work
```

## Conventions

- **Rust everywhere.** `cargo fmt` and `cargo clippy --all-targets -- -D warnings`
  must pass. Errors flow through `AppError`/`AppResult`. Keep `unwrap()`/`expect()`
  out of request/hot paths (mutex-poisoning expects are the only accepted ones).
- **The native UI split is fixed.** `phantom-gfx` owns terminal glyph rendering,
  cursor, selection, backdrop, and the terminal scrollbar. egui owns every other
  surface (settings, palette, panels). Do not rebuild egui widgets as ad-hoc
  terminal-renderer primitives, and do not let egui own PTY/grid state. See
  `ui-design-language.md`.
- Keep comments at the level the existing code uses: they explain *why*
  (especially macOS / wgpu / winit quirks), not *what*.
- New dependency → strongly prefer not to. If unavoidable: justify it, confirm
  the license is on `deny.toml`'s allow-list, and let the audits run.

## Required checks before declaring work done

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
cargo deny check          # if installed
cargo audit               # if installed
bash scripts/check-no-network.sh
```

Verify behavior in the real app with `cargo run -p phantom-app` when a change is
observable at runtime. Report failures honestly — never claim a check passed that
you did not run.

## When adding a feature

- New config field → add to `AppConfig` in `phantom-core/src/config.rs` (with
  `#[serde(default)]` and a sane default), **add bounds validation in
  `AppConfig::validate()`**, and surface it in the egui settings (`egui_ui.rs`)
  if user-facing.
- New execution behavior → it must resolve through a validated `ShellProfile`,
  an exact-source-bound trusted project task, or a reviewed built-in provider's
  fixed structured argv contract. Never add an arbitrary shell-command path.
- New backdrop / embedded asset → place it under the embedding crate's `assets/`
  dir and reference it with `include_bytes!`; keep the backdrop enum validated in
  Rust (no arbitrary paths or remote URLs through config).
