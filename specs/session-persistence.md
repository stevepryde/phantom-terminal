# Remembered session ownership

Status: confirmed product and security contract.

This document is the canonical owner for multi-instance behavior of Phantom's
remembered tab set. An explicit `--cwd` launch is ephemeral; all other launches
are normal launches.

## Requirements

- **R1 — Single owner.** At most one normal Phantom process may own, restore,
  or mutate the remembered tab set at a time.
- **R2 — Lock before state.** A normal launch must acquire a process-lifetime,
  crash-released operating-system advisory lock before it loads the session
  store or opens a window. If another process owns the lock, the launch must
  exit clearly with a nonzero status without reading, clearing, or replacing
  remembered tabs.
- **R3 — Ephemeral concurrency.** An explicit `--cwd` launch may run alongside
  the normal owner. It must not acquire the remembered-tabs lock and must not
  restore, save, or clear remembered tabs.
- **R4 — Store safety.** The lock must be safe across threads and processes,
  leave no stale ownership after a crash, and preserve the session store's
  owner-only permissions and symlink protections.
- **R5 — Canonical contract.** These numbered requirements are the stable
  product contract for remembered-session ownership. Changes to the behavior
  must update this document and its acceptance coverage together.

## Acceptance

Deterministic tests must use separately opened operating-system file handles
and stores to prove exclusion, release and reacquisition, unchanged remembered
state after a rejected secondary normal launch, and unchanged remembered state
after an ephemeral secondary launch. Lock-file permission and symlink checks
must use the same security expectations as the session database.
