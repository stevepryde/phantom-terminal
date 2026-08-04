---
name: phantom-workflows
description: Create, update, validate, or explain Phantom Terminal `.phantom.yml` project workflows. Use for project sidebar tasks, related services in separate tabs, repeated terminal setup, or invalid Phantom manifests.
---

# Phantom Workflows

Author the smallest version-one `.phantom.yml` that matches the project's real commands. Phantom owns review, trust, and execution.

## Workflow

1. Inspect repository instructions, documented commands, executable scripts, and any existing manifest.
2. Preserve unrelated tabs and stable IDs. Every `cwd` is relative to the manifest directory and must already exist within its canonical root.
3. Represent each independent service or useful shell as one tab.
4. Run `phantom context validate <manifest-directory>` and fix every error.
5. Report changed tabs and commands. Validation does not prove that services start.

## Version-one shape

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
  - id: shell
    title: Project shell
    cwd: .
```

`version`, `name`, and a non-empty `tabs` list are required. Each tab needs a unique identifier-like `id` and non-empty `title`; `cwd` defaults to `.`. Omitting `run` opens the user's validated default shell. `run` accepts only `program`, `args`, and `env`.

## Guardrails

- Use structured program and arguments. Never embed pipes, redirects, command substitution, `&&`, or `sh -c`.
- Do not create a wrapper merely to bypass structured argv.
- Never include secrets or credentials in `env`.
- Never use absolute or parent-traversing `cwd` values or create missing directories to satisfy validation.
- Do not run or trust tasks on the user's behalf; first-time or changed manifests require Phantom review.
- Do not invent dependencies, sequencing, readiness checks, restart policies, panes, or other unsupported orchestration fields.
- If shell semantics are essential, recommend a reviewed project-owned executable instead of weakening the manifest.
