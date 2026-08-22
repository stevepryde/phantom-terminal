# Phantom Terminal specifications

This directory is the canonical home for product and architecture requirements
that span multiple crates. Existing repository documentation remains normative
for the areas it owns:

- [`../AGENTS.md`](../AGENTS.md) owns security and engineering constraints.
- [`../ui-design-language.md`](../ui-design-language.md) owns the UI language.
- [`contextual-actions.md`](contextual-actions.md) owns directory-aware actions.
- [`skill-distribution.md`](skill-distribution.md) owns the bundled AI authoring
  skill and its local installer.
- [`scrollback-search.md`](scrollback-search.md) owns interactive in-memory
  terminal find behavior and its compact overlay.
- [`session-persistence.md`](session-persistence.md) owns remembered-tab
  behavior across multiple app instances.
- [`architecture.md`](architecture.md) records the relevant system boundaries.

Status labels in these documents distinguish confirmed user requirements from
observed implementation details and proposed future work.
