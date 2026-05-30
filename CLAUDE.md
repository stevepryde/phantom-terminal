# CLAUDE.md

This file guides Claude Code when working in this repository.

**Read [`AGENTS.md`](AGENTS.md) first** — it is the canonical contributor guide
(architecture, the security model, conventions, and required checks). Everything
there applies to you. This file only restates the priorities and the rules that
most often trip up automated edits.

## Priority order (non-negotiable)

When goals conflict, resolve strictly in this order:

1. **Security** — Phantom Terminal runs shells and handles secrets; the webview
   is an untrusted boundary. Never weaken the no-network posture, CSP,
   capability lockdown, supply-chain gates, or backend validation for
   convenience. If something cannot be done securely, stop and ask.
2. **Correctness** — behavior must match what the user and the code claim.
   Prefer a smaller, fully-correct change over a larger, subtly-wrong one. Don't
   leave half-wired features that only appear to work.
3. **Maintainability & testability** — clear boundaries, small units, tests for
   logic, no brittle coupling to library internals.

Performance is tracked in [`ROADMAP.md`](ROADMAP.md) but never overrides the above.

## Hard rules (do not violate without asking the user)

- Do **not** add an HTTP client, auto-updater, telemetry, or any network egress.
  `scripts/check-no-network.sh` enforces this in CI.
- Do **not** let `pty_spawn` accept an arbitrary command/args from the webview.
  Commands come only from a stored, validated `ShellProfile` (see `config.rs`).
- Every new config field **must** get bounds validation in
  `AppConfig::validate()` (`src-tauri/src/config.rs`). TS types are not a trust
  boundary.
- Do **not** loosen the CSP, add a capability, add an install/postinstall
  script (`trustedDependencies` stays empty), or unpin a GitHub Action.
- Prefer Rust and dependency-free solutions over new npm packages.

## Workflow

- Use **Bun** for JS tooling (the user's standing preference), and prefer Rust
  where a choice exists.
- `src/lib/ipc.ts` is the single source of truth for the IPC surface; keep it in
  sync with the Rust `serde` shapes (snake_case on the wire).
- Before declaring work done, run the full check suite listed in `AGENTS.md`
  ("Required checks") and verify runtime behavior with `bun run tauri dev` when
  the change is observable. Report failures honestly.

## Current state

See [`ROADMAP.md`](ROADMAP.md) for the prioritized assessment and the backlog of
fixes/improvements. The codebase is already strongly hardened; most open items
are correctness and maintainability polish, plus one notable PTY-throughput
performance item.
