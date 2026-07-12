# Architecture

## Confirmed boundaries

- `phantom-core` owns validated configuration, project-manifest parsing and
  trust records, filesystem discovery, and process launch specifications.
- `phantom-app` owns active-tab context, the built-in contextual plugin
  registry, background discovery orchestration, egui presentation, and action
  dispatch.
- `phantom-gfx` remains responsible only for terminal rendering. Contextual
  controls are an egui right sidebar below full-width titlebar chrome; its
  measured width reduces only the terminal grid while the backdrop renderer
  retains the full underlying pane.
- Context plugins are compiled-in Rust modules with typed discovery and action
  results. They are not dynamic libraries and cannot introduce network access.

## Execution trust boundary

Mutable project files are inert until approved. `.phantom.yml` is parsed and
validated into a typed proposal. Trust binds the canonical project root and a
copy of the exact bounded manifest source to stored, validated task definitions.
Changed bytes invalidate trust before any task can launch.

Project tasks and built-in integrations launch direct executable/argument
vectors. They never pass project text through `sh -c`. The sole PTY command
injection is the Recent directories provider's fixed `cd '<canonical path>'`
shape: the path comes from validated internal history, is re-canonicalized at
dispatch, and is POSIX single-quoted before submission. Selecting an action is
always explicit; discovery itself remains read-only.

## State and data flow

1. Active-tab cwd changes schedule background discovery with a generation id.
2. Enabled built-in providers inspect only the exact current directory.
3. Stale generation results are discarded.
4. Egui renders the resulting sections in a non-modal right sidebar and reports
   its measured physical width to terminal layout.
5. User actions are returned as typed outcomes to `App` for revalidation and
   dispatch.
6. Global/plugin/sidebar/section preferences, bounded directory history, and
   trusted task records persist in the validated `AppConfig` JSON already stored
   by `SessionStore`.

## Invariants

- Discovery never executes project code.
- Trust is invalid after a manifest content or canonical-root change.
- Relative manifest paths cannot escape the canonical project root, including
  through symlinks.
- A disabled plugin performs no discovery and renders no section.
- An idle contextual sidebar does not capture terminal keyboard input.
- Context task tabs are not restored as executable tasks after restart.
- Spdeploy operation discovery and execution use fixed executable/flag shapes;
  operation names come from spdeploy's structured listing output.
