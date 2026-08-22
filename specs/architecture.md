# Architecture

## Confirmed boundaries

- `phantom-core` owns validated configuration, project-manifest parsing and
  trust records, filesystem discovery, and process launch specifications.
- `phantom-app` owns active-tab context, the built-in contextual plugin
  registry, background discovery orchestration, egui presentation, and action
  dispatch. Its binary entrypoint also owns non-GUI CLI routing and installation
  of application-bundled agent assets.
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
vectors. They never pass project text through `sh -c`. Context providers never
inject synthesized commands into an existing PTY: directory navigation creates
an ordinary shell tab rooted at a re-canonicalized history path, and frequent
commands are non-interactive manual references. Selecting an action is always
explicit; discovery itself remains read-only.

The context validator is a read-only projection of this boundary. It calls the
same `phantom-core` manifest loading and trust-binding validation used before
launch, but discards the resulting typed value. It cannot grant trust or reach
the PTY manager.

## Bundled agent assets

The `phantom-app` crate embeds the complete `phantom-workflows` skill at compile
time. The CLI installer performs local filesystem writes only and never fetches
skill content from the network. Codex and Claude receive identical `SKILL.md`
instructions; product-specific metadata may be ignored by clients that do not
consume it. Installed skill files are an authoring aid only: they can create or
update `.phantom.yml`, but only Phantom's existing review and exact-source trust
flow can authorize execution.

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
- Spdeploy discovery descriptor-safely reads the bounded, sorted, root-relative
  full transitive graph of typed nested deploy configs. Operations remain inert
  until that exact graph and canonical root are stored as trusted. Dispatch
  rechecks persisted trust and rereads the graph before using the fixed
  spdeploy executable/flag shape; discovery never invokes a process.
