# Phantom workflow skill distribution

## Purpose

Let a user install an application-owned AI skill that can inspect a local
project and author its `.phantom.yml`, without adding an in-app AI system or
allowing an agent to bypass Phantom's execution review.

## Requirements

- **SKILL-001 (confirmed):** `phantom skill install` must install the bundled
  `phantom-workflows` skill for both Codex and Claude by default. `--target
  codex`, `--target claude`, and `--target all` must select an explicit target;
  `--force` must explicitly authorize replacement of Phantom-owned files in an
  unmanaged or locally modified collision.
- **SKILL-002 (security):** Skill installation must be entirely local and use
  bytes embedded in the compiled application. It must not perform network
  access, invoke either agent, modify project files, grant manifest trust, or
  execute a project task.
- **SKILL-003 (confirmed):** Codex installs under
  `${CODEX_HOME:-$HOME/.codex}/skills/phantom-workflows`. Claude installs under
  `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/phantom-workflows`.
- **SKILL-004 (correctness):** Installation must be repeatable. It must create a
  missing skill, leave byte-identical owned files unchanged, and use a local
  ownership marker and per-file digests to update an unchanged Phantom-managed
  install to the version bundled with the current binary. It must refuse an
  unmanaged or locally modified collision unless `--force` is supplied, leave
  unrelated and retired files untouched, retire removed paths from its marker,
  and report whether each target was installed, updated, or already current.
- **SKILL-005 (security):** The installer must reject a symlink where it expects
  the owned skill directory, owned subdirectory, or owned file. A failure must
  be reported with a non-zero exit status rather than following the link.
- **SKILL-006 (correctness):** An invalid command shape, unsupported target,
  missing home directory, or filesystem failure must produce a concise error
  and non-zero exit status without starting the GUI.
- **SKILL-007 (confirmed):** The skill must inspect current repository evidence
  before choosing commands, carefully merge an existing manifest, emit only the
  strict version-one structured `program`/`args`/`env` schema, keep every cwd
  relative and within the manifest root, and run `phantom context validate`
  after writing.
- **SKILL-008 (security):** The skill must not place shell command strings in a
  manifest, synthesize wrapper scripts to bypass structured argv, trust a
  manifest, or run configured tasks. It must tell the user that new or changed
  manifests require review in Phantom.
- **SKILL-009 (maintainability):** The bundled skill must pass the standard
  Codex skill-structure validator. Codex-facing metadata must remain consistent
  with `SKILL.md`; Claude may ignore that additional metadata.
- **SKILL-010 (confirmed):** Native installation must expose the `phantom` CLI.
  Linux continues installing the binary directly. macOS must create or update a
  `~/.local/bin/phantom` link to the installed app binary, with
  `CLI_INSTALL_DIR` as an override, while preserving any unrelated existing
  file or symlink and warning when the chosen directory is outside `PATH`.

## Acceptance criteria

- **AC-SKILL-A:** With temporary Codex and Claude homes, run `phantom skill
  install`; both receive identical bundled instructions and the command reports
  two successful installations.
- **AC-SKILL-B:** Run the same command again; both targets are reported current
  and no owned file contents change. Simulate an unchanged older managed bundle
  and confirm it updates automatically. Modify an owned file and confirm the
  installer refuses it until `--force` is supplied.
- **AC-SKILL-C:** Install each explicit target independently and confirm the
  unselected target is untouched. Invalid targets and missing home resolution
  exit non-zero without starting a window.
- **AC-SKILL-D:** Replace an expected skill directory or owned file with a
  symlink and confirm installation fails without changing the link target.
- **AC-SKILL-E:** Invoke the installed skill for a representative two-service
  repository. It derives structured commands from repository evidence, writes a
  valid manifest, validates it through Phantom, and leaves trust and execution
  to the user.
- **AC-SKILL-F:** Native installer syntax validation passes. On macOS, a clean
  temporary install destination produces a CLI link to the installed app binary;
  an unrelated existing CLI path is left unchanged. Documentation gives DMG
  users the installed app binary path because drag-to-Applications cannot mutate
  the user's shell path.

## Non-goals

- An in-app AI builder or network-backed assistant.
- Installing arbitrary third-party skills.
- Agent-specific divergent workflow semantics.
- A general-purpose shell workflow, dependency graph, readiness-check, or
  process-supervision language in `.phantom.yml` version 1.
