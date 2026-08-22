# Scrollback search

## Purpose

This spec owns interactive text search across the active terminal tab's
in-memory buffer. It covers matching, navigation, selection scope, keyboard
behavior, and the compact find overlay. The repository security model,
terminal/egui ownership boundary, and general visual rules remain owned by
[`../AGENTS.md`](../AGENTS.md) and
[`../ui-design-language.md`](../ui-design-language.md).

## Requirements

- **FIND-001 (confirmed):** `Ctrl+F` must open a find overlay for the active tab
  and focus its text input. Pressing `Ctrl+F` while it is already open must
  focus and select the current query so it can be replaced immediately.
- **FIND-002 (confirmed):** The default mode must perform case-insensitive
  literal substring matching across the active tab's in-memory scrollback and
  visible screen. Search data must never be persisted or sent over the network.
- **FIND-003 (confirmed):** The overlay must expose independent toggle buttons
  for case-sensitive, whole-word, regular-expression, and selection-only
  matching. Literal substring mode remains active when regular expression is
  off. Case and whole-word toggles also constrain regular-expression matching.
- **FIND-004 (confirmed):** Selection-only mode must search only the terminal
  selection that existed when the overlay opened. With no captured selection,
  it must produce no matches and explain that state through the option's
  tooltip; it must not silently fall back to the full buffer.
- **FIND-005 (confirmed):** Search must update as the query or any option
  changes. An empty query, an invalid regular expression, or a query with no
  matches must disable both Previous and Next. Invalid regular expressions must
  preserve the entered text and show a compact inline error state rather than
  closing the overlay or searching as a literal.
- **FIND-006 (confirmed):** A valid query with matches must highlight one active
  match and scroll it into view. Other visible matches must use a distinct,
  dimmer highlight without the active underline. Next and Previous must move
  through matches in buffer order, wrap at either end, and keep the active match
  visibly highlighted without replacing the user's ordinary terminal
  selection. `Enter` must activate Next and `Shift+Enter` Previous.
- **FIND-007 (confirmed):** `Escape` must close the overlay, return keyboard
  input to the terminal, and keep the viewport at the last visited match. The
  temporary search highlight must be removed; the ordinary terminal selection
  must remain unchanged throughout the interaction.
- **FIND-008 (confirmed):** Search state is per active interaction, not
  persisted. Switching tabs or closing the searched tab must close the overlay
  and remove the temporary match highlight from the original tab when it still
  exists.
- **FIND-009 (confirmed):** The overlay must be a compact, single-row egui
  surface at the top-right of the terminal pane. It must stay left of an open
  contextual sidebar. With the sidebar closed, it must leave enough right-side
  clearance for the floating sidebar button. It must not reserve terminal grid
  space or overlap titlebar/tab chrome.
- **FIND-010 (confirmed):** The row must contain only a search text box, the
  four option icons, a compact match count, Previous, and Next. The count must
  show the one-based active position and total as `N of M`, `0` when there are
  no matches, and `10000+` when matching reaches the configured cap. It must be
  a non-interactive accessible label. Controls must use Phantom's compact dark
  surface, `4px` rounding, visible focus/toggle states, accessible labels, and
  tooltips. Option icons must communicate state without relying on color alone.
  Previous and Next must be at least `28px` icon targets and visibly disabled
  when unavailable.
- **FIND-011 (correctness):** Matching must preserve a stable mapping from
  returned text spans to terminal cells, including Unicode text, wide cells,
  wrapped lines, and scrollback coordinates. Whole-word matching must use
  Unicode-aware word boundaries. Regex evaluation must be bounded by the
  existing finite in-memory terminal buffer and must not introduce a network
  capability.
- **FIND-012 (architecture):** Buffer extraction and terminal-coordinate
  mapping belong to `phantom-emu`; query/options/navigation state and input
  routing belong to `phantom-app`; the overlay belongs to egui; terminal cells,
  selection, and scrollbar rendering remain owned by `phantom-gfx`.

## Acceptance criteria

- **AC-FIND-A:** Populate more output than one viewport, press `Ctrl+F`, enter a
  mixed-case fragment, and observe case-insensitive matches in both visible and
  scrolled-off output. Visible non-active matches use a dimmer fill while the
  active match retains its stronger fill and underline. Next/Previous wrap and
  scroll each active match into view; `Enter` and `Shift+Enter` perform the same
  navigation.
- **AC-FIND-B:** Toggle case-sensitive and whole-word independently and observe
  the result set update immediately. A word embedded inside a longer Unicode
  word is excluded only when whole-word is active.
- **AC-FIND-C:** Enable regex, enter a valid expression, and navigate its
  matches. Enter an invalid expression and observe an inline invalid state with
  both navigation buttons disabled and no crash or literal fallback.
- **AC-FIND-D:** Select a buffer range before opening search, enable
  selection-only, and observe that matches outside the captured range are
  excluded. Repeat without a selection and observe zero available navigation.
  Close search and observe the original selection unchanged.
- **AC-FIND-E:** With the contextual sidebar open, verify that the find surface
  ends before the sidebar divider. Close the sidebar and verify that the find
  surface does not cover the floating reopen button. Resize to a narrow usable
  window and verify that controls remain contained in the terminal pane without
  covering titlebar/tab chrome.
- **AC-FIND-F:** While the overlay is focused, typed text and navigation keys do
  not reach the PTY. Press `Escape` and verify normal terminal typing resumes at
  once, the viewport remains at the last match, and temporary search highlight
  is gone.
- **AC-FIND-G:** Verify the overlay shows `N of M` while navigating matches, `0`
  for an empty result set, and `10000+` after reaching the match cap. The count
  remains in the single row and is announced as a non-interactive result label.
- **AC-FIND-H:** Verify literal and regex matching across Unicode, wide glyphs,
  wrapped terminal lines, and scrollback boundaries. Run the repository's
  required formatting, lint, build, test, supply-chain (when installed), and
  no-network checks.

## Design notes

- The captured selection is deliberately stable for the lifetime of the find
  interaction. The active find highlight is separate from the terminal's
  ordinary selection so selection-only remains deterministic and copy behavior
  is not changed by navigation.
- Active and non-active match state is derived in `phantom-emu` from the app's
  latest bounded async result set. `phantom-gfx` owns the two clipped paint
  styles; it does not repeat search or navigation logic.
- The overlay intentionally omits a close button. The compact result count and
  active terminal highlight provide steady-state feedback; `Escape` is the
  close action.
- Search is ephemeral and bounded by the configured in-memory scrollback, so it
  does not change Phantom's session-data or no-network posture.
