# Code review findings — native app stability & correctness

Date: 2026-06-12
Scope: full review of all four crates (~13.7k lines) for stability issues,
hangs, CPU/memory spikes, and correctness bugs. Findings were produced by
parallel subsystem reviews (app/event loop, PTY/core, renderer/gfx, UI chrome)
and every high-severity item was verified directly against the source.

Line numbers refer to the tree at commit `e6acac7`.

## Fix status (2026-06-12)

**All 24 findings fixed** on this branch: H1–H5, M1–M7, L1–L15.

Notes on the second batch:

- **L4** — the tab strip (horizontal and vertical) now scrolls: every tab gets
  a rect, the strip clamps a scroll offset, wheel over the strip scrolls it,
  switching/spawning/closing/reordering scrolls the active tab into view, and
  partially visible tabs are clipped at the strip edges (`strip_scroll`,
  `Rect::intersect`, renderer clip).
- **L8** — `parse_combo` rejects letter keys without Cmd/Ctrl/Alt and multiple
  non-modifier keys; on macOS `Ctrl` now binds the literal Ctrl key (distinct
  from `CmdOrCtrl`); the settings panel refuses to commit a draft whose
  bindings don't parse. Pre-existing configs with bare-letter bindings keep
  loading (core validation unchanged); the binding is inert and flagged
  "Invalid" in settings.
- **L11** — the palette closes on the configured `palette.toggle` chord
  (parsed from config, matched against egui input) and on a backdrop click;
  Escape still always closes.
- **L13** — the renderer has a clip rect (`set_clip`/`clear_clip`): quads are
  clamped at emit time with glyph UVs adjusted proportionally; the terminal
  grid draws under a viewport clip, so oversized fallback glyphs can't paint
  over the margin/scrollbar/chrome.
- **L14** — `resolve_glyph` iterates the already-deduplicated fallback order
  directly instead of building a candidates Vec with per-push `contains`.
- **Informational 1 (theme-list coupling)** — `UI_THEMES` now lives in
  `phantom-core` as the single source of truth: `AppConfig::validate()` checks
  against it and the app re-exports it for the settings picker and palette. A
  test guards that every listed theme has a dedicated accent colour.
- **Informational 2 (degenerate blur regions)** — `union_bounds` skips
  zero-size regions and returns `None` when nothing remains; `BlurPass::run`
  then exits before the surface snapshot and both blur passes instead of
  blurring the whole frame for nothing.

---

## High severity

### H1. Blocked PTY write freezes the entire app while holding the global sessions mutex

- **Files:** `crates/phantom-core/src/pty.rs:212-220`; callers in
  `crates/phantom-app/src/lib.rs:583, 1009, 1092, 1634, 1670`

```rust
pub fn write(&self, id: u32, data: &[u8]) -> AppResult<()> {
    let mut sessions = self.lock();
    ...
    s.writer.write_all(data)?;
    s.writer.flush()?;
```

`PtyManager::write` takes the single `sessions` lock and then performs a
blocking `write_all`/`flush` on the PTY master fd (portable-pty's
`UnixMasterWriter` is a plain blocking `Write`; no `O_NONBLOCK` anywhere).
Every call site runs on the winit event-loop thread: keystrokes, IME commit,
mouse reports, VT query replies, and — worst — paste
(`lib.rs:1670`), which writes the entire clipboard in one call.

The kernel PTY input buffer is small (~16–64 KB). If the foreground process
isn't reading stdin (suspended job, `Ctrl+S` flow control, busy process),
`write_all` blocks indefinitely: no redraws, no input, the close button stops
working (`CloseRequested` is never processed). Because the sessions mutex is
held for the duration, `resize`, `kill`, and `cwd` for **all** tabs wedge too,
and reader-thread EOF cleanup stalls.

Plausible permanent deadlock: paste >64 KB into a program that is
simultaneously emitting output → the 1 MB `PendingPtyEvents` cap
(`lib.rs:62, 133-157`) stalls the reader → the child blocks writing stdout →
the child never reads stdin → the UI thread never returns from `write_all` →
the UI never drains the pending queue. Only force-quit recovers.

**Fix:** Move writes off the UI thread — a per-session writer thread fed by a
bounded queue (mirroring the reader design), or non-blocking writes with a
pending-output buffer drained on wakeups. At minimum, never perform PTY writes
while holding the sessions map lock, and chunk paste data.

### H2. Child processes are never reaped on natural exit — one zombie per closed shell

- **File:** `crates/phantom-core/src/pty.rs:190-199`

```rust
sink.on_eof();
// Shell exited or pipe closed: drop the session.
let removed = sessions.lock()...remove(&id).is_some();
```

On EOF the reader thread removes the `PtySession` and drops the boxed
`std::process::Child` without calling `wait()` — dropping a `Child` does not
reap. The follow-up `PtyExit → close_tab → pty.kill(id)` path finds the
session already removed and no-ops. Empirically confirmed: spawning
`/bin/sh -c "exit 0"` through `PtyManager::spawn` leaves a `<defunct>` entry
after EOF. Every tab whose shell exits naturally (`exit`, Ctrl-D) leaks a
zombie for the app's lifetime; heavy tab churn accumulates them unboundedly.
The SIGKILL fallback inside `kill()` (portable-pty's `ChildKiller::kill`) also
never waits.

**Fix:** In the reader thread after EOF, call `child.wait()` before dropping
(the process is dead or dying, so it returns promptly). In `PtyManager::kill`,
follow the kill with a reap.

### H3. Startup blank-line filter withholds all output until the first `\n` — blank terminal for plain prompts

- **Files:** `crates/phantom-app/src/tab.rs:96` (filter),
  `tab.rs:156-188` (`scan_startup_bytes`); re-armed from
  `crates/phantom-app/src/lib.rs:1749-1757`

```rust
StartupScan::NeedMore if self.pending.len() <= Self::MAX_PENDING => Vec::new(),
```

`scan_startup_bytes` only reaches a decision on a literal LF (or a truncated
escape); plain visible text falls through to `NeedMore`, so `filter` buffers
everything (up to 4096 bytes). A shell whose startup output is just a prompt
with no newline — plain `bash-5.2$ `, `sh`, `dash` — renders a completely
blank tab, and each typed character's echo is also buffered. Everything
appears only when Enter's `\r\n` echo arrives or pending exceeds 4 KB.

Worse: `apply_terminal_grid` (`lib.rs:1749-1757`) calls
`tab.expect_prompt_repaint()` for **every tab on every grid resize**, re-arming
the filter mid-session — after a window resize, up to 4 KB of output from any
tab (e.g. a TUI repaint that uses cursor positioning rather than LF) is
withheld until more output arrives.

**Fix:** Treat end-of-buffer with `saw_visible && !saw_erase` as `Keep`
rather than `NeedMore` — the heuristic only needs to examine leading
whitespace/escapes. The existing done-path covers erase-then-LF arriving in a
later chunk.

### H4. `strip_repaint_blank_line` runs on all output forever and deletes legitimate newlines

- **File:** `crates/phantom-app/src/tab.rs:90-92, 111-148`

```rust
if self.done {
    return strip_repaint_blank_line(bytes);
}
...
b'\n' if saw_erase && !saw_visible_since_erase => {
    let mut out = bytes.to_vec();
    out.remove(index);
```

Once the startup filter is done, **every** subsequent PTY chunk for the tab's
entire lifetime is scanned, and the first `\n` following a CSI `J`/`K` erase
with only control characters in between is silently deleted. That byte pattern
(`…\x1b[K\r\n`) occurs in ordinary output — progress bars finishing a line,
pagers clearing to EOL, ncurses `clrtoeol()` — so real lines get merged or
overwritten. The corruption is also chunk-boundary dependent (scan state
resets per call and chunks are coalesced arbitrarily in
`push_pending_pty_event`, `lib.rs:182-200`), so identical byte streams corrupt
or not depending on read timing. It also copies every chunk
(`bytes.to_vec()`) on the hot output path.

**Fix:** Gate the strip behind the explicit armed/one-shot state (what
`arm()`/`expect_prompt_repaint()` exists for) — one decision after arming,
never on steady-state output. Return borrowed bytes when unchanged.

### H5. Glyph atlas exhaustion permanently blanks new glyphs — no eviction, growth, or recovery

- **Files:** `crates/phantom-gfx/src/atlas.rs:139-188` (`GlyphAtlas::insert`),
  `crates/phantom-gfx/src/lib.rs:31` (`const ATLAS_SIZE: u32 = 2048;`)

```rust
} else {
    GlyphEntry::empty(glyph.left, glyph.top, glyph.is_color)
};
self.cache.insert(key, entry);
```

When the shelf packer is full, the glyph is cached as `empty` **forever** (the
doc comment admits: "so we don't retry every frame"). There is no eviction, no
second atlas page, no growth, and no reset — the atlas is a single 2048×2048
texture for the lifetime of the `Renderer` (rebuilt only on font/theme/scale
config change). At typical HiDPI sizes (~32 px glyphs) capacity is roughly
3,500–6,000 glyphs. A CJK session (3–5 k common ideographs) or heavy
emoji/Nerd-Font output can exhaust it, after which every *newly seen*
character renders as an invisible blank cell — no log, no error, no recovery
short of changing font settings. Silent data loss in a terminal.

Contributing factor — `ShelfPacker::alloc` (`atlas.rs:38-46`) mutates shelf
state before the failing height check, abandoning the previous shelf's
remaining width even when the allocation then fails, which accelerates
exhaustion.

**Fix:** At minimum, reset the atlas (clear cache, new packer) when an alloc
fails so live glyphs re-pack on subsequent frames (cheap — rasterization
results stay cached), and log once when the atlas fills. Better: multiple
atlas pages or growth up to the device texture-size limit. In
`ShelfPacker::alloc`, compute the candidate position into locals and only
commit on success.

---

## Medium severity

### M1. Surface format hardcoded; `Lost` recovery panics; `Validation` freezes silently

- **File:** `crates/phantom-app/src/gpu.rs:90, 109, 182-193`; retry mapping in
  `crates/phantom-app/src/lib.rs:2099-2105`

Three related problems in the present path:

1. `surface.get_capabilities(&adapter)` is fetched but only `alpha_modes` is
   consulted; the format is hardcoded to `Bgra8UnormSrgb`. On backends whose
   surface doesn't list it (GL/GLES fallbacks common when Vulkan drivers are
   missing on Linux typically expose only `Rgba8UnormSrgb`),
   `surface.configure` raises a validation panic at startup.
2. `CurrentSurfaceTexture::Lost` recreates the surface with
   `.expect("recreate surface")`. `Lost` typically fires around GPU
   resets/driver restarts — exactly when `create_surface` is most likely to
   fail — turning a recoverable hiccup into a crash of an app holding live
   shell sessions.
3. `CurrentSurfaceTexture::Validation` returns `PresentStatus::Fatal`, which
   `surface_retry_delay` maps to *no retry and no exit*: rendering silently
   stops while the window (and shells) stay alive — a permanently frozen but
   live window.

**Fix:** Pick `Bgra8UnormSrgb` if present in `capabilities.formats`, else the
first sRGB format, else `formats[0]` (renderer/blur/egui already take the
format as a parameter). For `Lost`, propagate `create_surface` failure as a
status instead of panicking. For `Fatal`, retry a bounded number of times, then
surface an error and exit cleanly (flushing persistence).

### M2. `PtyManager::kill` sleeps up to ~250 ms on the UI thread while holding the sessions mutex

- **File:** `crates/phantom-core/src/pty.rs:237-243`; callers
  `crates/phantom-app/src/lib.rs:846` (`close_tab`), `lib.rs:1819-1821`
  (`request_exit`)

```rust
if let Some(mut s) = self.lock().remove(&id) {
    let _ = s.child.kill();
```

portable-pty's `kill()` sends SIGHUP, then polls `try_wait` with 50 ms sleeps
(up to 5 attempts) before SIGKILL. The crates are **edition 2021**, where the
`MutexGuard` temporary in an `if let` scrutinee lives until the end of the
block — so the sessions mutex is held across those sleeps, stalling
reader-thread cleanup and every other PTY call. Closing a tab whose child
ignores SIGHUP freezes the UI ~200–250 ms; quitting with N such tabs blocks
N×250 ms serially in `request_exit`.

**Fix:** Bind `self.lock().remove(&id)` to a local first (guard dropped), then
kill outside the lock — preferably on a detached background thread, which is
also the natural place for the H2 reap.

### M3. Invalid settings drafts are applied to the live config (validate round-trip bypassed)

- **Files:** `crates/phantom-app/src/egui_ui.rs:208-214`,
  `crates/phantom-app/src/lib.rs:1868-1870, 941-953`

```rust
if changed {
    match config.validate() {
        Ok(()) => self.notice = None,
        Err(error) => self.notice = Some(error.to_string()),
    }
}
changed   // returned true regardless of validation outcome
```

`settings_panel` mutates the live `self.config` in place and returns
`changed = true` even when `validate()` fails; `render` then runs
`apply_config_change()` — rebuilding the keymap and window chrome from the
invalid config and firing a save. Practically reachable via the keybinding
`keys` `TextEdit`: clearing the field mid-retype applies an empty binding to
the runtime `Keymap`, and every keystroke fires a background save that fails
with a notice. Disk is protected (`SessionStore::save_config` re-validates and
rejects), but in-memory config diverges from disk until the user types a valid
value; quitting at that point silently reverts. This violates the project rule
that a draft edited in an egui widget must round-trip through `validate()`
before being applied.

**Fix:** Edit a draft copy of `AppConfig` (or at least of `keybindings`) and
only assign into `self.config` / report `config_changed` when `validate()`
passes; on failure keep the inline notice without applying or saving.

### M4. Glyph rasterization failure retried per cell, per frame — font file re-mapped and re-parsed each time

- **Files:** `crates/phantom-gfx/src/lib.rs:489-495` (`emit_glyph`),
  `crates/phantom-gfx/src/font.rs:187-197` (`rasterize`)

```rust
None => self
    .font
    .rasterize(resolved)
    .map(|raster| self.atlas.insert(queue, key, &raster)),
```

`resolve_glyph` caches its result (including `None`), but a `rasterize`
failure inserts nothing into the atlas cache, so the next frame misses
`atlas.get` and retries. `rasterize` goes through `fontdb::with_face_data`,
which re-maps the font file from disk and re-parses it with
`FontRef::from_index` on every call. If a face fails to parse (corrupt font,
or the file was deleted/replaced while running), every visible occurrence of
that glyph triggers a file map + parse attempt every frame — a sustained CPU
spike scaling with grid coverage.

**Fix:** On `rasterize(..) == None`, insert `GlyphEntry::empty(0, 0, false)`
into the atlas cache for that key, mirroring the atlas-full path.

### M5. `FontSet::new` panics are reachable mid-session (scale change / settings apply)

- **Files:** `crates/phantom-gfx/src/font.rs:124-127, 141-142`; caller
  `crates/phantom-app/src/lib.rs:972-984` (`rebuild_renderer`)

```rust
.expect("no usable font found on this system");
...
let metrics = metrics_for_face(&db, faces[regular].id, size_px, line_height)
    .expect("regular face failed to parse");
```

`Renderer::new → FontSet::new` runs not only at startup but from
`rebuild_renderer`, which fires on `WindowEvent::ScaleFactorChanged` and on
debounced font-setting changes. `resolve_primary_face` can fall back to
`first_face(db)` — any face in the DB — and if that face fails
`FontRef::from_index`, the app panics mid-session, e.g. when dragging the
window to another monitor.

**Fix:** Make `FontSet::new` return `Result`; on failure `rebuild_renderer`
keeps the old renderer and shows a notice. When the picked regular face fails
to parse metrics, iterate further candidates instead of expecting the first to
parse.

### M6. Tab drag / click state survives asynchronous tab removal — acts on the wrong tab

- **Files:** `crates/phantom-app/src/lib.rs:841-861` (`close_tab`),
  `lib.rs:1403-1409` (`on_mouse_up`), `lib.rs:1139-1172` (`handle_click`),
  `lib.rs:1201-1216` (`reorder_tab`), `crates/phantom-app/src/chrome.rs:319-323`
  (`TabHit`)

`close_tab` adjusts `self.active` but never touches `self.tab_drag` or
`self.scroll_drag`, and click handling resolves last frame's hit rects by
index. A `PtyExit` event (shell exits on its own) can arrive mid-drag or
between render and click; the stored indices then point at shifted positions,
so mouse-up reorders/activates the wrong tab, and `Hit::Close(i)` /
`Hit::Switch(i)` can close or switch a neighboring tab. Everything is
bounds-checked — no panic, just wrong behavior. A tab switch during a
scrollbar drag similarly leaves `scroll_drag.start_offset` applying to a
different tab.

**Fix:** Store the dragged/hit tab's `id: u64` instead of its index and
resolve to an index at action time; cancel `scroll_drag` when the active tab
changes.

### M7. egui repaint requests are dropped (`repaint_delay` ignored)

- **File:** `crates/phantom-app/src/egui_ui.rs:419-439` (`EguiLayer::run`);
  deadline machinery at `crates/phantom-app/src/lib.rs:2356-2374`

`run` consumes `platform_output`, `shapes`, `textures_delta`, and
`pixels_per_point`, but never reads `full_output.viewport_output[..]
.repaint_delay`, and nothing else handles `request_repaint`. egui-driven
animation (settings/palette text caret blink, hover fades) only advances when
some other event triggers a redraw. Today the 530 ms terminal cursor-blink
timer masks it; with `cursor_blink` disabled the palette/settings UI is
visibly frozen while idle.

**Fix:** After `ctx.run_ui`, read the root viewport's `repaint_delay`; if
zero, request an immediate redraw, otherwise feed it into the `min_deadline`
computation in `about_to_wait` like the blink timer.

---

## Low severity

### L1. Config file rewritten ~60×/sec during slider drags through an unbounded channel

- **File:** `crates/phantom-app/src/lib.rs:223` (unbounded `mpsc::channel`),
  `lib.rs:1868-1870`

Dragging a settings slider produces a `SaveConfig` message per frame, each
cloning the full `AppConfig` into an unbounded channel; the worker rewrites
the config file for each. Slow disk → unbounded queue growth; fast disk →
needless write amplification. **Fix:** debounce saves (a `SAVE_DEBOUNCE`
already exists for renderer rebuilds) or coalesce in the worker by draining
the channel and keeping only the last `SaveConfig`/`SaveTabs`.

### L2. Occluded window wakes the event loop every 250 ms indefinitely

- **Files:** `crates/phantom-app/src/gpu.rs:177`,
  `crates/phantom-app/src/lib.rs:2000-2002, 2336-2341, 2356-2359`

`Occluded` schedules a retry in 250 ms, which attempts a frame, gets
`Occluded` again, and reschedules — forever — while cursor blink additionally
wakes the loop every 530 ms and renders a full frame even when hidden. The
process never idles while hidden; steady battery cost. **Fix:** track
`WindowEvent::Occluded(true/false)`, suspend blink/redraw scheduling while
occluded, resume on un-occlude (keep a single retry as a safety net).

### L3. Wide chars (CJK/emoji) advance one cell in chrome text — IME preedit overlaps

- **Files:** `crates/phantom-gfx/src/lib.rs:346-357` (`text`), `lib.rs:301-303`
  (`text_width`); IME caller `crates/phantom-app/src/lib.rs:1955-1962`

The grid path handles `cell.width == 2` correctly, but `Renderer::text`
advances exactly one cell per `char` and `text_width` counts
`chars() * cell_w`. Most visible in IME preedit — the primary input path for
CJK users — where double-width glyphs overlap and the highlight box is too
narrow. Also affects CJK tab titles and notices. **Fix:** advance by
`cell_w * wcwidth(ch)` (alacritty's `unicode-width` is already in the tree via
`phantom-emu`) and mirror in `text_width`.

### L4. Overflowing tabs are invisible and mouse-unreachable; drop index computed against visible tabs only

- **File:** `crates/phantom-app/src/chrome.rs:772-778`
  (`horizontal_tab_rects_with_widths`), `chrome.rs:505-518` (`drop_index`)

Once tabs shrink to `MIN_TAB_W` and still don't fit, the remainder get no
rect, no `TabHit`, no close button — unreachable by mouse (only shortcuts or
the palette reach them), and the active tab itself can be invisible.
`drop_index` returns the visible count for "drop at end", so dragging a tab to
the end of a crowded bar inserts it before the hidden tabs. No panic
(`reorder_tab` clamps). **Fix:** render an overflow indicator/count, or allow
widths below `MIN_TAB_W` so every tab always has a hit rect; clamp drop
semantics intentionally.

### L5. `SCROLLBAR_MIN_THUMB_H` is not DPI-scaled

- **File:** `crates/phantom-app/src/chrome.rs:29, 231`

```rust
let thumb_h = (track.h * visible_fraction).clamp(SCROLLBAR_MIN_THUMB_H.min(track.h), track.h);
```

Every other chrome dimension goes through `px(layout, ..)`, but the minimum
thumb height clamps against the raw logical constant — at 2× DPI the minimum
thumb is half the intended size. Looks like a miss from commit `e6acac7`
("Scale Phantom chrome geometry for high-DPI displays"). **Fix:** pass the
scale factor into `scrollbar_thumb` and use
`(SCROLLBAR_MIN_THUMB_H * scale).min(track.h)`.

### L6. Session restore misattributes titles and active index on spawn failure

- **File:** `crates/phantom-app/src/lib.rs:762-777`

`spawn_tab_with_persistence` can fail without pushing a tab; the restore loop
then assigns `rec.title` to `tabs.last_mut()` — the *previous* record's tab —
and the record-index `active` drifts from actual tab indices (clamped, so no
panic; wrong tab renamed/selected). **Fix:** return success/the pushed index
from `spawn_tab_with_persistence` and only apply title/active bookkeeping on
success.

### L7. Escape closes the settings panel even when a text field has focus

- **File:** `crates/phantom-app/src/egui_ui.rs:143-145`

The check reads raw input, ignoring widget focus. Escape is egui's standard
"release focus" key for `TextEdit`; escaping out of the keybinding field
closes the whole panel (possibly with a half-typed invalid binding already
applied per M3). **Fix:** skip when
`ui.ctx().memory(|m| m.focused().is_some())`.

### L8. Keybinding parsing edge cases

- **File:** `crates/phantom-app/src/keybindings.rs:54-77`

- `parse_combo("t")` is accepted (no modifier) and `Keymap::lookup` matches it
  — a bare-letter binding silently hijacks plain typing into the terminal.
  Consider requiring at least one modifier (or a named/F key) for `Char`
  bindings.
- `"ctrl" | "cmd" | ... => primary = true`: on macOS a literal `Ctrl+T`
  binding actually fires on **Cmd**+T, and real Ctrl combos can't be bound.
  Document or split `ctrl` from `cmdorctrl`.
- Multiple non-modifier tokens (`"A+B"`) silently keep only the last.

### L9. No SQLite `busy_timeout`; concurrent instances fail saves and stomp each other

- **File:** `crates/phantom-core/src/session.rs:72-75`

Two running Phantom instances share the WAL DB; concurrent writes return
SQLITE_BUSY immediately → "Could not remember tabs" notices, and `save_tabs`'s
DELETE-then-reinsert means last-writer-wins between windows. Also, the DB file
is briefly created with default-umask permissions before
`restrict_db_permissions` chmods it to 0600 (the 0700 parent dir mitigates).
**Fix:** set `conn.busy_timeout(...)`; consider an instance guard; create the
DB with 0600 from the start (umask or open-then-chmod-before-write).

### L10. `Renderer::end` concatenates and re-uploads all instance data every frame

- **File:** `crates/phantom-gfx/src/lib.rs:520-544`

Two fresh heap concatenations per frame (potentially ~1 MB at large grids)
followed by a full `write_buffer`, even when content is identical (cursor-blink
redraws re-upload the entire grid). **Fix:** issue two `write_buffer` calls at
offsets directly from `self.solids[0]`/`[1]` — no concat Vec needed.

### L11. Palette close chord hardcoded; app-side palette key handler is dead code

- **Files:** `crates/phantom-app/src/egui_ui.rs:688-692`,
  `crates/phantom-app/src/lib.rs:1020-1037, 2298-2302`

While the palette is open, `palette_owns_input` blocks `handle_input`
entirely, so the keymap-driven palette branch never runs — only the overlay's
hardcoded Escape / Cmd+K paths close it. Rebinding `palette.toggle` away from
Cmd/Ctrl+K means the rebound chord no longer closes an open palette; clicking
the backdrop also does nothing. Not a stuck state (Escape always works), but
inconsistent. **Fix:** pass the configured combo into the overlay; treat
backdrop clicks as close.

### L12. Scrollbar hover highlight doesn't request a repaint

- **Files:** `crates/phantom-app/src/lib.rs:1518-1528` vs `lib.rs:1943-1953`

`render()` computes `scrollbar_active` from the live cursor position, but
entering/leaving the scrollbar track doesn't request a redraw when the chrome
hover target stays `None` — the highlight appears only on the next incidental
repaint (530 ms blink tick; indefinitely stale with `cursor_blink` off).
**Fix:** track `scrollbar_hovered: bool` and `request_redraw()` when it flips.

### L13. Glyph quads aren't clipped to the terminal viewport

- **File:** `crates/phantom-gfx/src/lib.rs:381-471, 496-511`

Clipping is whole-cell only; oversized fallback glyphs (emoji from a non-mono
face, italic overhang, wide char in the last visible column) can paint outside
the viewport over margin/scrollbar/chrome since the base layer has no scissor.
**Fix:** scissor the terminal portion of the pass, or clamp glyph quads (and
UVs proportionally) to the clip rect.

### L14. First resolution of an uncovered codepoint scans every installed face

- **File:** `crates/phantom-gfx/src/font.rs:163-183, 340-371`

`fallback_faces` pushes every face in the DB and `push_unique` is a linear
`contains`, so for a char no font covers, each candidate does an mmap + parse
charmap probe — with ~1000+ faces (common on macOS) the first paint of exotic
text can stall a frame. Cached afterwards (including `None`), so a one-time
spike per `(slot, char)`. **Fix:** build candidates from the precomputed
`fallback_order` without per-push `contains`; consider capping the
whole-DB tail.

### L15. `ShellProfile.name` skips NUL/non-empty validation

- **File:** `crates/phantom-core/src/config.rs:152-173`

`validate()` checks `name` length but, unlike every sibling string field, has
no `validate_no_nul` and no non-empty check. Display-only impact
(palette/tab labels), but it's an inconsistency in the `AppConfig::validate()`
trust point.

---

## Informational

- `PaletteAction::SetUiTheme` (`lib.rs:906-909`) writes `config.ui_theme`
  without a validate round-trip. Safe today because values come from
  `themes::UI_THEMES`, which mirrors the validated list, but the coupling is
  manual (`themes.rs:3` documents it). A debug assertion or shared constant
  would harden it.
- `blur.rs:420-422`: if all blur regions clamp to zero size, `union_bounds`
  falls back to the full surface, so the horizontal pass blurs the entire
  frame for nothing (output is still correct). An early-out for all-degenerate
  regions would save GPU work.

---

## Verified clean

Areas explicitly checked and found sound:

- **PTY backpressure:** the pending-events queue is bounded at 1 MB with a
  correct condvar protocol (no lost-wake race), per-tab byte coalescing, 8 KB
  reads (not per-byte), and `close()` unblocks readers at exit.
- **Redraw scheduling:** `request_redraw` is debounced; `about_to_wait` uses
  `Wait`/`WaitUntil` with a min deadline; `ControlFlow::Poll` is restricted to
  the 120 ms live-resize grace window. No idle redraw storm.
- **Zero-size surface:** all configure paths clamp to ≥ 1×1.
- **UTF-8 chunk boundaries:** raw bytes flow into alacritty's stateful vte
  parser; no lossy decode anywhere.
- **Spawn trust boundary:** the only `PtyManager::spawn` caller sources
  command/args exclusively from validated `ShellProfile`s; restore-path
  profile ids re-resolve against current config; no UI edits profile commands.
- **`AppConfig::validate()` coverage:** every field bounds-checked except the
  L15 nit.
- **SQLite off hot paths:** all saves go through the background persistence
  worker; saves trigger on state changes, never per keystroke/frame;
  `save_tabs` is transactional over WAL.
- **DPI/scale invalidation:** `ScaleFactorChanged → rebuild_renderer`
  recreates `FontSet` + atlas + bind group together; no stale-DPI glyphs, no
  texture leak on resize.
- **Layout math:** grid size clamped to 1..=1000; chrome rects clamped ≥ 0;
  scrollbar/scroll-drag divisions guarded; `close_tab` active-index math
  correct; `blur.rs::clamp_rect` cannot underflow; truncation helpers are
  char-boundary safe.
- **Thread shutdown:** reader threads exit on EOF/queue close; the persistence
  worker exits when its channel closes and `flush()` cannot hang if the worker
  died.

## Suggested fix order

1. **PTY layer** (H1, H2, M2) — same file; one redesign of write/kill/reap
   covers all three.
2. **Tab output filter** (H3, H4) — same small state machine in `tab.rs`.
3. **Atlas recovery** (H5, M4) — same `emit_glyph`/`insert` path; fix
   together.
4. **Surface robustness** (M1) and the remaining mediums, then lows
   opportunistically.
