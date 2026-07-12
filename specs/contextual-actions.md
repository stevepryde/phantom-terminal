# Contextual directory actions

## Purpose

Provide non-modal, directory-aware workflows for creating related terminal tabs
and invoking local tools without weakening Phantom Terminal's no-network or
validated-execution posture.

## Requirements

- **CTX-001 (confirmed):** When the active tab enters a directory containing
  `.phantom.yml`, Phantom must show its manifest section in the right sidebar.
- **CTX-002 (confirmed):** An untrusted or changed manifest must show a
  non-blocking review surface with its exact task programs, arguments,
  environment values, and working directories. It must execute nothing until
  the user explicitly trusts it.
- **CTX-003 (confirmed):** Trust must bind the canonical project root and exact
  manifest contents. Editing the file must require renewed approval.
- **CTX-004 (confirmed):** An approved manifest section must offer creating all
  declared tabs and creating each tab individually. A tab may run a typed task
  automatically after creation or open only the configured shell.
- **CTX-005 (confirmed):** Closing the sidebar must release all terminal space
  and leave a floating icon at the top-right of the main content area. Clicking
  the icon reopens the sidebar. Open/closed state must survive restart.
- **CTX-006 (confirmed):** Every plugin section must be independently
  collapsible and separated from adjacent sections by a horizontal divider.
- **CTX-007 (confirmed):** Settings must include Context Actions controls for
  globally enabling the feature and independently enabling built-in plugins.
- **CTX-008 (confirmed):** A built-in spdeploy plugin must detect `deploy.yml`
  in the exact active directory, internally parse only the project name,
  operation names/descriptions, and single-stage submenu paths needed to build
  the dropdown, and launch the selected operation in a new tab only after
  explicit user action. Discovery must not require or invoke the spdeploy CLI.
- **CTX-009 (confirmed):** Both trust review and approved action surfaces must
  remain non-modal. Terminal input must continue unless the user is directly
  interacting with a contextual control.
- **CTX-010 (confirmed):** Context plugins must use a common typed provider
  model so further compiled-in providers can be added without changing the
  generic section renderer.
- **CTX-011 (security):** Navigation and discovery must never execute a task,
  shell string, deploy operation, or project-provided program.
- **CTX-012 (security):** Manifest schema, file size, collection counts, text,
  environment keys, and resolved paths must be bounded and validated before
  display, trust, storage, or launch.
- **CTX-013 (correctness):** Parse/discovery/tool errors must appear inside the
  affected section and must not crash, block, or disable the terminal.
- **CTX-014 (confirmed):** Context actions must use a resizable right sidebar,
  with a `100px` minimum and a maximum of `50%` of the live viewport. It must
  reserve terminal text and interaction space rather than cover terminal
  content, and its chosen width must persist. The terminal backdrop image must
  continue beneath the translucent sidebar.
- **CTX-016 (confirmed):** The sidebar's default presentation must be
  action-first and compact, without redundant panel headings. Paths and command
  metadata belong behind Details; text must remain comfortably readable.
- **CTX-015 (security):** An untrusted manifest must not expose its Trust action
  until the user expands the exact task details for review.
- **CTX-017 (confirmed):** The open sidebar is confined to the main content area
  below the titlebar. The titlebar and horizontal tab chrome must extend to the
  right window edge. The sidebar remains open when no directory-specific
  actions exist.
- **CTX-018 (confirmed):** A built-in Recent directories plugin must select the
  five most recently visited directories and the five most frequently visited
  directories and remove duplicates. Selection uses recency and visit frequency;
  presentation order is defined separately by CTX-023.
- **CTX-019 (confirmed):** Clicking a directory row sends a safely quoted `cd`
  to the active terminal session. Shift-clicking opens a new tab rooted at that
  directory. Directory rows elide the start of long paths so the basename stays
  visible.
- **CTX-020 (confirmed):** Context sections are compact accordions separated by
  a single thin horizontal divider. Headers are fixed 26px rows with smaller
  labels and a leading chevron that points right when collapsed and down when
  expanded. Built-in headings are Tasks, Deploy, and Directories. Action rows
  are fixed at 28px and directory rows at 24px, with 12px primary text, no gaps,
  a full-row hover state, and a full-row click target. The sidebar contains no
  instructional help copy.
- **CTX-021 (confirmed):** The sidebar's entire left divider is the resize
  handle. It must expose a horizontal-resize cursor, visibly emphasize hover and
  drag state, and update the persisted width continuously while dragging.
- **CTX-022 (confirmed):** Every context plugin has a validated numeric order.
  Directories occupies the first stable slot; context-dependent Tasks and Deploy
  plugins retain deterministic later slots whether or not their sections are
  currently present. Discovery completion order must never reorder sections.
- **CTX-023 (confirmed):** After selecting the five most recent and five most
  frequent directories and removing duplicates, the displayed directory list is
  sorted case-insensitively by path with the original path as a deterministic
  tie-breaker.
- **CTX-024 (correctness):** Maximize, restore, and live window resizing clamp
  the rendered sidebar width without changing its persisted preferred width.
  Full-surface backdrop blur is suppressed during intermediate resize frames and
  restored once at the settled surface size so ordinary window transitions
  cannot trigger unbounded GPU resource churn.
- **CTX-025 (confirmed):** The contextual sidebar always remains translucent,
  even when the shared panel-opacity setting is 100%, and the terminal backdrop
  continues beneath it. Directory labels replace the current user's exact home
  prefix with `~` before start-elision; navigation continues to use the original
  canonical path.
- **CTX-026 (confirmed):** The expanded Deploy section contains no redundant
  project/directory-name, Details row, or `Operation` field label. It starts
  directly with the dropdown. Each selectable dropdown leaf displays `name:
  description`, falling back to `name` when no description exists.
  Nested submenu breadcrumbs appear as non-selectable group headings. Dispatch
  still uses the validated operation name and exact declaring config, including
  when different submenu files declare operations with the same name.
- **CTX-027 (correctness):** Errors and other global notices render in egui's
  topmost layer after every panel, sidebar, palette, and popup. Contextual UI
  must never obscure an error.
- **CTX-028 (correctness):** Explicit contextual programs are resolved from the
  normalized GUI-safe PATH before PTY spawn. The fallback includes `~/bin`,
  `~/.cargo/bin`, and `~/.local/bin`, so Finder-launched builds can run trusted
  tools without inheriting a login shell's environment.

## `.phantom.yml` version 1

```yaml
version: 1
name: Soulfire
tabs:
  - id: api
    title: Soulfire API
    cwd: soulfire-api
    run:
      program: cargo
      args: [run]
      env:
        RUST_LOG: info,soulfire=debug
  - id: ui
    title: Soulfire UI
    cwd: soulfire-ui
    run:
      program: ./serve.sh
  - id: deploy
    title: Deploy
    cwd: .
```

Version 1 accepts only the documented fields. `cwd` is relative to the manifest
directory. `run` is an executable plus argument vector and environment map, not
a shell command string. Omitting `run` opens the default validated shell.

## Acceptance criteria

- **AC-CTX-A:** Entering a valid fixture directory shows the manifest review;
  leaving it removes the section; returning shows the appropriate trust state.
- **AC-CTX-B:** Trusting a fixture and choosing Open all creates its ordered
  tabs at their resolved directories; task tabs run their typed task and the
  prompt-only tab runs the default shell.
- **AC-CTX-C:** Changing one manifest byte prevents task launch until renewed
  trust.
- **AC-CTX-D:** Collapse the panel and each section, restart, and observe the
  same states. Disabled plugins remain undiscovered and hidden.
- **AC-CTX-E:** A `deploy.yml` fixture produces structured operations; selecting
  one launches fixed spdeploy argv without bypassing spdeploy confirmations.
- **AC-CTX-F:** Malformed, oversized, traversing, symlink-escaping, unknown-key,
  or unsupported-version manifests produce a bounded local error and no launch.
- **AC-CTX-G:** Typing into the terminal while an idle contextual panel is
  visible reaches the PTY unchanged.
- **AC-CTX-H:** At a typical desktop width the sidebar begins compact, can be
  resized from `100px` through `50%` of the viewport using its left edge, and
  restores that width after a restart. Compact mode omits full paths and command
  metadata; Details reveals them without changing the primary action flow.
- **AC-CTX-I:** Terminal glyphs, selection, cursor, mouse mapping, and scrollbar
  stop before the sidebar, while the backdrop image remains visible through it.
- **AC-CTX-J:** The titlebar reaches the right edge above an open sidebar. Close
  the sidebar and observe no reserved rail, then click its floating content-area
  icon and observe the sidebar return at its persisted width.
- **AC-CTX-K:** Revisiting and repeatedly using fixture directories produces a
  deduplicated combined recent/frequent list. A click changes the active shell's
  directory; Shift-click creates a new tab at the same path.

## Non-goals

- Runtime-loaded third-party code or plugins.
- Network discovery, telemetry, or package installation.
- Automatic execution on `cd`, application startup, or manifest detection.
- Reimplementing spdeploy validation, variables, stage semantics, or execution;
  Phantom's YAML reader extracts only the fields needed to list leaf actions.
