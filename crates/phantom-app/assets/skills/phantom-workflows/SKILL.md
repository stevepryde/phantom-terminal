---
name: phantom-workflows
description: Create, update, validate, or explain Phantom Terminal `.phantom.yml` project workflows. Use when an agent needs to add project-specific sidebar tasks, launch related services in separate tabs, migrate repeated terminal setup into Phantom, or repair an invalid Phantom manifest.
---

# Phantom Workflows

Author the smallest valid version-one `.phantom.yml` that matches the project's
real development commands. Phantom, not the agent, owns review, trust, and task
execution.

## Authoring workflow

1. Read the applicable repository instructions and inspect current evidence for
   the requested commands. Prefer documented development commands, executable
   scripts, Cargo metadata, and existing task-runner configuration over guesses.
2. Choose the manifest directory with the user request in mind. Every tab cwd
   is relative to that directory and must already exist within its canonical
   root.
3. Read an existing `.phantom.yml` before editing. Preserve unrelated tabs and
   stable ids; merge only the requested changes.
4. Write strict version-one YAML using the schema below. Use one independent tab
   per long-running service or useful shell.
5. Run `phantom context validate <manifest-directory>` after writing. Fix every
   validation error before reporting completion.
6. Report the tabs and commands added or changed. Remind the user that Phantom
   requires first-time or renewed review before anything can run.

## Version-one schema

```yaml
version: 1
name: Project Name
tabs:
  - id: api
    title: API
    cwd: services/api
    run:
      program: cargo
      args: [run]
      env:
        RUST_LOG: info
  - id: ui
    title: UI
    cwd: apps/ui
    run:
      program: ./serve.sh
  - id: shell
    title: Project shell
    cwd: .
```

Required manifest fields are `version`, `name`, and non-empty `tabs`. Each tab
requires a unique identifier-like `id` and non-empty `title`. `cwd` defaults to
`.`. Omitting `run` opens the user's validated default shell in that directory.
`run` accepts only a `program`, an argument list, and an environment map.

## Guardrails

- Never put a shell command string, pipe, redirect, command substitution, `&&`,
  or `sh -c` in the manifest. Split executable and arguments structurally.
- Never generate a wrapper script merely to bypass the structured argv model.
- Never include secrets, access tokens, passwords, or private keys in `env`.
  Reference the project's existing secure environment-loading behavior instead.
- Never use absolute or parent-traversing cwd values. Do not create missing task
  directories just to make validation pass.
- Never trust the manifest or run its tasks on the user's behalf. Validation is
  read-only and is not evidence that a service successfully starts.
- Do not invent dependencies, readiness checks, restart policies, sequential
  steps, panes, or other orchestration fields. Version one describes independent
  tabs only.
- If the requested workflow cannot be represented without shell semantics,
  explain the limitation and recommend a reviewed project-owned executable or a
  future typed Phantom feature instead of weakening the manifest.
