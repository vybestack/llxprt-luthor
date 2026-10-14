# Issue #67: interrupted source operations

## Changes

Startup records source reconciliation for every attempt-free unfinished task.
Claim completion is checked against the saved intent and configured login;
observable assignment can finish a missing evidence row without reassigning.
Worktree recovery uses the exact pinned base and verifies checkout common Git
directory, registration, path, task branch, tip and cleanliness. Missing paths
may be created; conflicting paths or branches are never removed or reset.

Verified prelaunch tasks can reach initial dispatch, with the saved config and
fresh claim, PR absence and authenticated author checks. Source holds retain
capacity but are task-local; unresolved stops and attempt/state errors stay
global. Launch intents are never replayed automatically. Abandon requires
exclusive worktree ownership and no running/unverified attempt (the existing
strict never-dispatched proof permits retiring an unlaunched intent).

Schema v4 allows replacement selections only after abandonment while preserving
all original task, attempt, reservation, intent and evidence rows. Its live issue
identity index and active reservation uniqueness remain enforced. Migration is
transactional, with foreign keys enabled throughout.

## Regressions

`tests/coordinator/source_interruptions.rs` kills an independent process with
SIGKILL after selection, claim intent, claim evidence (before the phase update),
worktree intent, Git creation (before completion evidence), worktree evidence,
and launch intent. The next cycle either recovers or holds only that task, and
another issue reaches the launcher without reassigning the interrupted issue.
Held checkpoints have an audited abandon path. Additional tests cover dirty,
wrong-tip/branch, partial, foreign, symlink and absent-but-branch-present paths;
repeatable claim confirmation; prelaunch abandonment/reselection; and unresolved
stop safety. Migration tests verify history, foreign keys, uniqueness and full
rollback on a conflicting migration table. Existing regression assertions were
updated only where issue #67 deliberately changes source-recovery behavior.

## Validation environment

Rust 1.98.0, stable worktree cwd and absolute confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

Relative fixture state roots preserve production Unix-socket limits. Logs are
under ignored `tmp/issue67/`. No contracts, gates, lint/debt ceilings, timeouts or
safety thresholds are changed. Focused issue67, claim, state, worktree and CLI
source-recovery tests, structural policy and strict Clippy pass. The full unchanged
`cargo +1.98.0 xtask ci` result is recorded in the PR body.

One early broad focused call hit the tool's 600-second wall-clock cap during an
unrelated paused-resume fixture; its exact focused rerun passed. Subsequent long
validation is detached to avoid tool process-group termination.

The final detached full gate passed (exit 0), including all unchanged workspace,
doc, policy and contract checks. The first full attempt exposed one outdated CLI
claim-reconciliation expectation, corrected to assert the newly required durable
claim completion; the complete CLI suite then passed. The final full run passed
without timing-test failures or reduced checks.
