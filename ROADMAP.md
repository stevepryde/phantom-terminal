# Phantom Terminal — Codebase Assessment & Roadmap

_Assessed 2026-05-30 against commit `2785ebc`._

This is a status assessment and an actionable backlog. Items are ordered by the
project's priority: **Security → Correctness → Maintainability/Testability →
Performance**. Each item has enough detail (files, line refs, approach,
acceptance) for another agent to implement it without re-deriving the context.

## Overall verdict

This is a **mature, unusually security-conscious codebase** for its size (~1.6k
Rust LOC, ~3.7k TS LOC). The trust model is well chosen: the Rust backend owns
PTYs, the SQLite store, and all validation, and the webview is treated as
untrusted. Supply-chain hygiene (frozen lockfiles, empty `trustedDependencies`,
7-day release-age cooldown, `cargo-deny`/`cargo-audit`/`osv-scanner`/`bun audit`,
SBOMs, SHA-pinned Actions, no-network CI gate) is stronger than most production
apps. Rust validation in `config.rs`/`session.rs` is thorough and well tested.

The gaps are mostly **correctness polish** (a vestigial feature, optimistic
state with no rollback), **maintainability** (a 464-line `App.tsx`, brittle
reach-ins to a pre-release dep's internals), **test coverage of the UI/command
layer**, and **one real performance issue** (PTY bytes cross IPC as JSON number
arrays). None of these undermine the security posture.

Scores (1–5, higher is better): Security **5**, Correctness **3.5**,
Maintainability **3.5**, Testability **3**, Performance **3**.

---

## P0 — Security

The posture is already strong. These are hardening items, not open holes.

### SEC-1 — No cap on concurrent PTY sessions (local DoS)
- **Where:** `src-tauri/src/pty.rs` `PtyManager::spawn` (≈L61); `commands.rs::pty_spawn`.
- **Problem:** `pty_spawn` has no upper bound. A compromised or buggy webview can
  call it in a loop, spawning unbounded shells + reader threads (one
  `std::thread::spawn` each) — a fork-bomb reachable over IPC.
- **Fix:** Enforce a max live-session count (e.g. 256) in `PtyManager::spawn`;
  return `AppError::Pty("too many terminals")` past the cap. Optionally also cap
  threads by moving the reader pumps onto a bounded pool, but the count cap is
  the priority.
- **Acceptance:** spawning past the cap fails cleanly; a unit test asserts the
  limit; normal usage is unaffected.

### SEC-2 — IPC errors leak internal detail to the webview
- **Where:** `src-tauri/src/error.rs::command_error` (L23) stringifies full
  `AppError` — including `io error: …` (paths) and `sqlite error: …`.
- **Problem:** Low risk for a local app, but it hands the untrusted webview
  internal filesystem paths and DB internals; unnecessary for the UI.
- **Fix:** Map errors to stable, user-safe messages at the IPC boundary; log the
  detailed error to stderr/tracing only. Keep `InvalidConfig`/`Pty` messages
  (already user-facing and safe); generalize `Io`/`Sqlite`/`Json`.
- **Acceptance:** webview-visible strings contain no host paths or DB internals;
  full detail still reachable in logs.

### SEC-3 — Document & bound the `config_set` → `pty_spawn` trust path
- **Where:** `commands.rs::config_set` (L84) + `pty_spawn` (L13); `pty.rs` env build.
- **Problem (by design, worth documenting):** the webview cannot pass a command
  to `pty_spawn` directly, but it *can* write an arbitrary `command`/`args` into
  a profile via `config_set` and then spawn it — so a compromised webview can run
  arbitrary processes without the user typing. This is inherent to a terminal;
  the mitigations are CSP + no-network + minimal deps + the WASM sandbox.
- **Fix:** (a) Add a short "Trust model" note to `AGENTS.md`/code comments making
  this explicit so it is a deliberate, reviewed property. (b) Consider validating
  that a profile `command`, if non-empty, is an absolute path or resolves on
  `PATH` before spawn, and reject obviously hostile values — defense in depth,
  not a guarantee.
- **Acceptance:** the property is documented; optional command sanity-check has
  tests. Do **not** break the legitimate "empty command = $SHELL" path.

### SEC-4 — Terminal escape-sequence parsing is the main runtime attack surface
- **Where:** `ghostty-web@0.4.0-next.14` (pre-release), parsed in WASM; written
  to in `TerminalView.tsx` (`term.write(bytes)`, L105).
- **Problem:** Untrusted program output (escape sequences) is parsed by a
  **pinned pre-release** WASM lib. The WASM sandbox bounds blast radius, but
  pre-release parsers are where terminal CVEs live.
- **Fix:** Track `ghostty-web` releases; move off `-next.*` to a stable release
  when available; keep it on the dependabot radar. No code change today — this is
  a watch item. (Pairs with MAINT-2's internals coupling.)
- **Acceptance:** tracked; revisited whenever ghostty-web publishes a stable line.

---

## P1 — Correctness

### COR-1 — Keybindings config is vestigial (half-wired feature)
- **Where:** `config.rs` (`default_keybindings`, validated + persisted), mirrored
  in `src/lib/ipc.ts` (`Keybinding`), but the actual handler in
  `src/App.tsx` `onKey` (L194–247) **hardcodes** every shortcut and never reads
  `config.keybindings`. There is also no keybindings editor in `SettingsView`.
- **Problem:** The config stores and validates keybindings that have **no effect**.
  This is misleading state — a feature that looks implemented but is not.
- **Fix (pick one):**
  1. **Wire it up:** parse `config.keybindings[].keys` (e.g. `CmdOrCtrl+T`) into a
     matcher and drive `onKey` from config, with the current hardcoded set as the
     default; add a Keybindings section to `SettingsView`. Keep the
     `CmdOrCtrl`/platform mapping centralized.
  2. **Remove it:** drop `keybindings` from `AppConfig`, `default_keybindings`,
     validation, and `ipc.ts` until it's actually built (migrate existing stored
     configs — `load_config` already backs up + resets on validation failure, but
     an unknown field is ignored by serde, so removal is safe).
- **Recommendation:** Option 1 (it's a flagship feature of a Warp-like terminal),
  but Option 2 is acceptable to stop the dishonesty cheaply.
- **Acceptance:** either changing a keybinding in settings changes behavior, or
  the unused config is gone. No silent no-op feature remains.

### COR-2 — Optimistic config updates with no rollback or user feedback
- **Where:** `src/App.tsx::updateConfig` (L106–115). Applies the patch to React
  state immediately and fire-and-forgets `configSet(next)`, only `console.error`
  on rejection.
- **Problem:** The backend re-validates (`save_config` → `AppConfig::validate`).
  If the user types an invalid hex color (`SettingsView` `ColorInput`/selection
  field) or any out-of-range value, the UI keeps and live-applies the invalid
  value while persistence **silently fails** — in-memory and on-disk state
  diverge, and the bad value is lost on next launch with no explanation.
- **Fix:** On `configSet` rejection, roll back to the last persisted config and
  surface a non-blocking error (inline field error or toast). Alternatively,
  validate in the frontend before applying (mirror the Rust bounds) so the UI
  never shows a value the backend will reject. Backend remains the source of truth.
- **Acceptance:** an invalid setting is either prevented or visibly rejected and
  rolled back; persisted and displayed config never silently diverge.

### COR-3 — Font-size slider range disagrees with backend bounds
- **Where:** `SettingsView.tsx` AppearanceSection slider `min={8} max={32}` (L182)
  vs `config.rs` `MAX_FONT_SIZE = 48` (L8).
- **Problem:** Minor — the UI can't reach valid sizes 33–48. Not a bug, but an
  inconsistency that will confuse future edits.
- **Fix:** Make the slider `max={48}` (or lower `MAX_FONT_SIZE` to 32 if 48 was
  never intended). Pick one source of truth and align.
- **Acceptance:** UI range == backend-accepted range.

### COR-4 — `ptyWrite`/`ptyResize` rejections are swallowed
- **Where:** `TerminalView.tsx` `term.onData` → `ptyWrite(...)` (L122) and
  `onResize` → `ptyResize(...)` (L125); `App.tsx` cwd-poll `ptyCwd` (L255).
- **Problem:** These IPC calls return promises that are never awaited/caught. If
  the PTY died, writes reject and vanish — the terminal looks alive but input is
  silently dropped, with no exit handling (note the dangling `onExit` comment at
  `TerminalView.tsx` L232).
- **Fix:** Implement PTY-exit handling: when the reader pump ends (it already
  removes the session in `pty.rs` L141) signal the frontend (e.g. send a
  zero-length sentinel or a dedicated exit channel) so the tab can show "process
  exited" and stop accepting input. At minimum, `.catch` these calls and mark the
  tab as dead.
- **Acceptance:** killing the shell (`exit`) visibly reflects in the tab; no
  silent input loss.

---

## P2 — Maintainability & Testability

### MAINT-1 — `App.tsx` is a 464-line god-component
- **Where:** `src/App.tsx`.
- **Problem:** One component owns launch bootstrap, keyboard shortcuts, debounced
  session saving, cwd polling, display-layout refresh, window-shape tracking, and
  rendering. Hard to test, easy to break with cross-effect interactions.
- **Fix:** Extract custom hooks, each independently testable:
  `useKeyboardShortcuts(handlers)`, `useSessionPersistence(saver)`,
  `useLiveCwdPolling()`, `useDisplayLayoutRefresh()`, `useWindowShape()`. Keep
  `App.tsx` as composition + layout only.
- **Acceptance:** `App.tsx` < ~150 lines; each hook has a focused unit test;
  behavior unchanged.

### MAINT-2 — Brittle reach-ins to ghostty-web internals
- **Where:** `TerminalView.tsx` `refreshTerminalDisplay`/`forceTerminalRender`
  (L235–280) cast `term as unknown as TerminalInternals` to poke `.renderer`,
  `.wasmTerm`, `.viewportY`, `metrics`, `baseline`.
- **Problem:** Tightly coupled to private internals of a **pre-release** dep
  (`0.4.0-next.14`). A patch bump can silently break line-height/rendering with no
  type error. Untestable.
- **Fix:** Isolate every internals access behind one adapter module (e.g.
  `terminal/ghosttyAdapter.ts`) with a documented, narrow surface and runtime
  guards. Upstream the line-height capability to ghostty-web if possible so the
  cast can be dropped. Add a smoke check that fails loudly if an expected internal
  is missing after a version bump.
- **Acceptance:** exactly one file touches ghostty internals; a version bump that
  removes an internal fails fast with a clear message rather than rendering wrong.

### MAINT-3 — Thin test coverage on the command + UI layer
- **Where:** Good coverage exists for `store/`, `lib/paths`, `lib/store`,
  `command-palette` (frontend) and `config.rs`/`session.rs`/`pty.rs` helpers
  (Rust). Gaps:
  - `commands.rs::validate_spawn_cwd` (the spawn input guard) is **untested**.
  - No tests for `App.tsx` logic (now extractable per MAINT-1) or `SettingsView`.
  - `pty.rs` shell/env/PATH builders are partly tested; `apply_account_env`,
    `default_shell`, `shell_path` ordering are not.
- **Fix:** Add Rust unit tests for `validate_spawn_cwd` (too long, NUL, ok, None)
  and for the PATH/env builders (pure functions — easy). After MAINT-1, add
  Bun tests for the extracted hooks (esp. keyboard matching and session-save
  debounce).
- **Acceptance:** the IPC input validators and env builders have direct tests; CI
  green.

### MAINT-4 — Duplicated cwd resolution / two cwd-tracking loops
- **Where:** `App.tsx` polls `ptyCwd` for every tab every 1.5s (L254–263) **and**
  `TabSessionSaver` resolves cwd again on every save (`tabPersistence.ts`
  `buildTabRecords` L105). Both call `setTabCwd`.
- **Problem:** Redundant work and two sources of cwd truth; not buggy, but
  confusing and wasteful (N IPC round-trips/1.5s).
- **Fix:** Single owner for live cwd — let the poller be the only writer of
  `tab.cwd`, and have the saver read the store's `tab.cwd` instead of resolving
  again (or vice-versa). Consider event-driven cwd updates over polling if/when
  the backend can emit on `cd`.
- **Acceptance:** one cwd-resolution path; fewer IPC calls; tab labels still track
  `cd`.

### MAINT-5 — Minor duplication & docs
- `restrict_file_permissions`/`restrict_dir_permissions` in `session.rs` are
  near-identical (mode differs) — fold into one `set_mode(path, mode)`.
- `account_env()` is recomputed (getpwuid / env reads) several times per spawn
  inside `shell_path` → `expand_home_in_path_list` (`pty.rs` L262–305). Compute
  once and thread it through (also a micro-perf win).
- Add a one-paragraph "Trust model" section to `AGENTS.md` (see SEC-3).

---

## P3 — Performance

### PERF-1 — PTY bytes cross IPC as JSON number arrays (highest-impact)
- **Where:** `src/lib/ipc.ts`: `spawnPty` channel `Uint8Array.from(msg)` where
  `msg: number[]` (L127–129) and `ptyWrite` sends `Array.from(data)` (L133);
  backend sends `Channel<Vec<u8>>` (`pty.rs` L134) and reads `data: Vec<u8>`
  (`commands.rs` L49).
- **Problem:** Terminal output and input are serialized as JSON arrays of integers
  — roughly one JSON token per byte in **both** directions. For high-throughput
  output (`cat bigfile`, build logs, `yes`) this is a major CPU/GC cost and the
  most likely source of perceived lag.
- **Fix:** Move PTY data onto a binary transport. Tauri v2 supports raw byte
  payloads (e.g. `tauri::ipc::Response`/`InvokeResponseBody::Raw`, and
  `ArrayBuffer`/`Uint8Array` invoke args) without per-byte JSON. **Measure first**
  (throughput before/after with a large `cat`), then convert both directions to
  raw bytes / ArrayBuffer. Keep the `Channel` abstraction in `ipc.ts`.
- **Acceptance:** large-output throughput materially improves in a measured
  benchmark; no behavioral change to terminal I/O.

### PERF-2 — `path_helper` subprocess on every spawn (macOS)
- **Where:** `pty.rs::macos_path_helper_path` (L308) runs `/usr/libexec/path_helper`
  per `spawn`, and `account_env()` is re-resolved multiple times per spawn.
- **Problem:** Extra subprocess + syscalls on every new tab. Small, but the merged
  PATH is effectively process-constant.
- **Fix:** Compute the merged `shell_path()` (and `account_env`) once and cache for
  the process lifetime (e.g. `OnceLock`). Invalidation isn't needed — PATH/account
  don't change within a run.
- **Acceptance:** new-tab spawn does no redundant subprocess/getpwuid work; output
  identical.

### PERF-3 — cwd polling cost scales with tab count
- **Where:** `App.tsx` L254–263 (1.5s interval × every tab) — overlaps MAINT-4.
- **Fix:** Folded into MAINT-4 (single cwd path; consider longer interval or
  event-driven updates). Low priority on its own.

---

## Quick-reference: suggested order of execution

1. **SEC-1** (session cap) and **SEC-2** (error sanitization) — cheap, real hardening.
2. **COR-1** (keybindings) and **COR-2** (config rollback) — stop misleading behavior.
3. **PERF-1** (binary IPC) — biggest felt-performance win; measure around it.
4. **MAINT-1/2/3** — decompose `App.tsx`, isolate ghostty internals, backfill tests.
5. **COR-3/4, MAINT-4/5, PERF-2/3** — polish.

Keep every change behind the full check suite in `AGENTS.md` and never regress
the security model.
