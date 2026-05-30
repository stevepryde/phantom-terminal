# AGENTS.md — Phantom Terminal

Guidance for any AI agent (or human) working in this repository. Read this
before making changes.

Phantom Terminal is a Warp-like terminal: a **Tauri 2** desktop app with a
**Rust** backend (`src-tauri/`) and a **React 19 + TypeScript + Vite** frontend
(`src/`), managed with **Bun**. The Rust side owns the PTYs, the SQLite session
store, and all config validation; the webview is treated as untrusted input.

## Priority order (non-negotiable)

When priorities conflict, resolve them in this order. This ordering governs
design decisions, review focus, and what gets fixed first.

1. **Security** — this app runs shells and handles secrets. The webview is an
   untrusted boundary. Never weaken the no-network posture, the CSP, the
   capability lockdown, the supply-chain gates, or backend input validation to
   make a feature easier. A feature that cannot be built securely does not ship.
2. **Correctness** — behavior must match what the user and the code claim.
   Prefer a smaller feature that is fully correct over a larger one that is
   subtly wrong. No half-wired features that pretend to work.
3. **Maintainability & testability** — clear module boundaries, small units,
   tests for logic, no brittle coupling to library internals. Code is read far
   more than it is written.

Performance matters and is tracked in `ROADMAP.md`, but it never overrides the
three above. Do not trade away security or correctness for speed.

## The security model (do not regress)

This is the project's defining characteristic. Preserve every item here.

- **No outbound network, by design.** No HTTP client, no auto-updater, no
  telemetry. Enforced in CI by `scripts/check-no-network.sh`, which fails the
  build if a network-capable crate, a `tauri-plugin-(http|updater|websocket)`,
  an `http/shell/fs/dialog/upload` capability, or a remote CSP origin appears.
  Updates happen by rebuilding from source (`bun run update`).
- **The webview is untrusted.** All IPC commands live in
  `src-tauri/src/commands.rs`. The webview can never name an arbitrary
  executable to `exec` directly — `pty_spawn` resolves the command/args from a
  **stored, validated `ShellProfile`** by id (see `config.rs`). Keep it that
  way: do not add a command/args passthrough to `pty_spawn`.
- **Shell profiles are the reviewed execution path.** A compromised webview that
  can call `config_set` can change a profile and then ask `pty_spawn` to launch
  it. That is inherent to a terminal app, so the security boundary is the
  combination of strict CSP/no-network/minimal capabilities plus exhaustive Rust
  validation before profiles are stored or used. Treat any change to profile
  validation or spawn resolution as security-sensitive.
- **All config is validated in Rust before use or storage.**
  `AppConfig::validate()` in `src-tauri/src/config.rs` bounds every field
  (lengths, ranges, enums, hex colors, NUL bytes, profile/keybinding counts).
  Any new config field MUST get validation in the same pass. The frontend's
  TypeScript types are convenience, not a trust boundary.
- **Capabilities are minimal.** `src-tauri/capabilities/default.json` grants
  only `core:default` plus four window-control permissions. Adding a permission
  is a security decision — justify it in the file's `description` and confirm
  `check-no-network.sh` still passes.
- **CSP is strict** (`src-tauri/tauri.conf.json`): `'self'` + wasm only, no
  remote origins. `dragDropEnabled: false`. Do not loosen.
- **Supply chain is gated.** Frozen lockfiles, empty `trustedDependencies` (no
  install scripts run), a 7-day `minimumReleaseAge` cooldown (`bunfig.toml`),
  `cargo-audit` + `cargo-deny` + `osv-scanner` + `bun audit`, SBOM generation,
  and GitHub Actions pinned to full commit SHAs. The advisory ignore lists in
  `src-tauri/deny.toml` and `osv-scanner.toml` must stay in sync and only ever
  list *unmaintained/unsound transitive Tauri deps*, never real vulnerabilities.
  Prefer Rust and dependency-free solutions over adding npm packages (e.g.
  `src/lib/store.ts` is a hand-rolled store that exists to avoid a dependency).
- **Session data is protected at rest.** The SQLite store
  (`src-tauri/src/session.rs`) is created `0600` in a `0700` dir, with
  `secure_delete=ON`. Terminal scrollback is in-memory only and never persisted.
  Keep it that way.

If you believe a security control should change, stop and ask the user first.

## Repository layout

```
src/                    React + TS frontend
  lib/ipc.ts            The ONLY bridge to Rust commands; mirror Rust types here
  lib/store.ts          Dependency-free observable store (do not replace w/ a dep)
  lib/paths.ts          cwd → tab-label formatting
  store/                Tab state + session-persistence logic (well tested)
  terminal/             TerminalView (ghostty-web glue) + ligatures
  tabs/, settings/, command-palette/, components/   UI
src-tauri/src/
  commands.rs           #[tauri::command] handlers — the IPC trust boundary
  config.rs             AppConfig + exhaustive validation
  session.rs            SQLite session/config store
  pty.rs                PTY lifecycle, shell env, cwd lookup
  error.rs              AppError → IPC string
scripts/                no-network check, SBOM, local install
.github/workflows/ci.yml   security-first CI (lint, audit, build, sbom)
ROADMAP.md              prioritized findings & improvement plan — read this
```

## Conventions

- **Bun** for all JS tooling (`bun install`, `bun test`, `bun run …`). Not npm/yarn/pnpm.
- **Biome** for format + lint (2-space, double quotes, semicolons, width 100).
  Run `bun run lint` and `bun run format`. Justify every `biome-ignore` inline.
- **TypeScript is strict** (`noUnusedLocals/Parameters`, `noFallthrough`). No `any`
  in new code; the existing `as unknown as TerminalInternals` casts in
  `TerminalView.tsx` are a known debt (see ROADMAP) — do not add more.
- **Rust**: `cargo fmt`, `cargo clippy -- -D warnings` must pass. Errors flow
  through `AppError`/`AppResult`; commands return `Result<_, String>` via
  `command_error`. Keep `unwrap()`/`expect()` out of request paths (mutex
  poisoning expects are the only accepted ones).
- **`src/lib/ipc.ts` is the single source of truth for the IPC surface.** Every
  new command and every type that crosses the boundary is declared there and
  must match the Rust `serde` shape (snake_case on the wire).
- Keep comments at the level the existing code uses: they explain *why*
  (especially WKWebView/macOS quirks), not *what*.

## Required checks before declaring work done

Run what CI runs (see `README.md` "Checks"):

```sh
bun run typecheck
bun run lint
bun run test
bun run build
(cd src-tauri && cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings)
(cd src-tauri && cargo build --locked && cargo test --locked)
bash scripts/check-no-network.sh
```

Verify behavior in the real app with `bun run tauri dev` when a change is
observable at runtime. Report failures honestly — never claim a check passed
that you did not run.

## When adding a feature

- New IPC command → add the handler in `commands.rs`, register it in
  `lib.rs`'s `invoke_handler!`, mirror its types in `src/lib/ipc.ts`.
- New config field → add to `AppConfig` (with `#[serde(default)]` and a sane
  default), **add validation in `AppConfig::validate()`**, mirror in
  `ipc.ts`, and surface it in `SettingsView.tsx` if user-facing.
- New dependency → strongly prefer not to. If unavoidable: justify it, confirm
  the license is on `deny.toml`'s allow-list, keep `trustedDependencies` empty,
  and let the `minimumReleaseAge` cooldown and audits run.
