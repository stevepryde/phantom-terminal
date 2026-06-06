# CLAUDE.md

This file guides Claude Code when working in this repository.

**Read [`AGENTS.md`](AGENTS.md) first** — it is the canonical contributor guide
(architecture, the security model, conventions, and required checks). Everything
there applies to you. This file only restates the priorities and the rules that
most often trip up automated edits.

## Priority order (non-negotiable)

When goals conflict, resolve strictly in this order:

1. **Security** — Phantom Terminal runs shells and handles secrets. Never weaken
   the no-network posture, the supply-chain gates, the at-rest data protections,
   or backend validation for convenience. If something cannot be done securely,
   stop and ask.
2. **Correctness** — behavior must match what the user and the code claim.
   Prefer a smaller, fully-correct change over a larger, subtly-wrong one. Don't
   leave half-wired features that only appear to work.
3. **Maintainability & testability** — clear boundaries, small units, tests for
   logic, no brittle coupling to library internals.

## Hard rules (do not violate without asking the user)

- Do **not** add an HTTP client, auto-updater, telemetry, or any network egress.
  `scripts/check-no-network.sh` enforces this in CI.
- Do **not** let `pty_spawn` accept an arbitrary command/args. Commands come only
  from a stored, validated `ShellProfile` (see `phantom-core/src/spawn.rs` and
  `config.rs`).
- Every new config field **must** get bounds validation in
  `AppConfig::validate()` (`phantom-core/src/config.rs`). A draft value edited in
  an egui widget is not validated until it round-trips through `validate()`.
- Do **not** persist terminal scrollback or any terminal content to disk (only
  tab title/cwd/order is stored), and do **not** unpin a GitHub Action.
- Prefer Rust and dependency-free solutions over adding a crate.

## Workflow

- Rust only — there is no JS tooling. `cargo fmt`, `cargo clippy --all-targets
  -- -D warnings`, `cargo build/test --workspace`.
- The native UI split is fixed: `phantom-gfx` renders the terminal; egui renders
  every other surface. Don't blur that boundary (see `ui-design-language.md`).
- Before declaring work done, run the full check suite listed in `AGENTS.md`
  ("Required checks") and verify runtime behavior with `cargo run -p phantom-app`
  when the change is observable. Report failures honestly.

## Current state

The codebase is strongly hardened. It is a single native Rust process (winit +
wgpu + egui over a VT core); the former Tauri/webview frontend has been removed.
Most open work is correctness and maintainability polish.
