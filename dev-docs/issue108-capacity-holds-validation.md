# Issue 108: increase capacity while retaining permanent historical holds

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/108

## Scope and safety boundary

`resize-capacity` can now increase an existing queue's limit when its only
unaccounted launches are permanent historical initial-attempt holds under #27.
The read-only classifier requires a held task, exactly one unknown initial
attempt and original launch, a reserved slot, no contemporaneous preparation,
no terminal/recovery/retry/repair/amendment evidence, and an incomplete original
dispatch inventory. Modern prepared attempts and complete dispatch inventories
still require reconciliation. Presence of malformed preparation or complete
but malformed dispatch proof does not become an absence-based exception.

This exception is **increase-only**. Every historical reservation remains
reserved and must fit within the new limit. It does not authorize continuation,
retry, abandonment, release, or historical evidence reconstruction. Coordinator,
process/group absence, worktree-owner and ordinary capacity-mismatch checks are
unchanged. No ownership/worktree implementation or #27 release path changed.
No private queue/configuration, executable or live attempt was inspected or
modified; all predecessor evidence is synthetic and hermetic.

Each audit and CLI result includes `permanent_holds`, with the task ID, attempt
ID and reason for each counted hold in stable task/attempt order. Schema version
8 adds this JSON array to `capacity_changes`. Existing audits gain `[]` without
changing their old fields. The hold list and limit update commit together; SQL,
serialization and process-proof errors remain errors. There is no operator
attestation, forced release, schema-conflict adoption or fallback.

## Behavioral regressions

Ten new resize tests cover:

- A historical-only increase through the real CLI, durable named audit, status,
  restart, unchanged task/attempt/preparation/intent/evidence/reservation rows and
  owner bytes, and continued refusal of release/retry/undispatched continuation.
- A historical partial dispatch with a real reaped child registration, and two
  counted holds in deterministic order.
- Refusal with a second active launch, a prepared held launch, or a complete
  historical dispatch inventory awaiting reconciliation.
- Refusal of decreases (including decreases at the reservation count), stale
  counts, conflicting/missing owners, live registered processes and invalid
  process proof; database errors are not successful classifications.
- Atomic rollback of audit/limit on a write failure; v7 audit migration with old
  values preserved; refusal and rollback of a conflicting migration schema.

A further regression in the existing #27 integration binary exercises both real
planner-generated predecessor classes, proves the modern prepared versions
refuse resizing, then proves the historical versions can increase capacity while
all repeated reconciliation/recovery/retirement/abandonment/resume/retry/release
refusals and original history remain intact. Existing fixture process-isolation
and confined-relative Unix-socket paths are retained.

The capacity fixture now shares its typed launch-plan JSON between pending and
completed attempts so status assertions exercise valid persisted plans rather
than the old `original-plan` placeholder. Existing assertions were not relaxed.
Schema assertions now expect version 8; the unsupported-next-version test checks
9. No test, timeout, threshold, debt ceiling, contract, required check or safety
rule was removed or weakened. Capacity-audit migration was factored as one
coherent helper to remain within the unchanged complexity ceiling.

## Validation environment and results

Rust **1.98.0**, macOS aarch64, stable task-worktree cwd:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
cargo +1.98.0 fetch --locked
cargo +1.98.0 test --offline --locked --test capacity_resize --test historical_hold --test state -- --test-threads=1
cargo +1.98.0 xtask policy
cargo +1.98.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity -D clippy::type_complexity
cargo +1.98.0 xtask ci
```

All dependencies, build outputs, fixtures and raw logs stay beneath this
worktree's ignored `tmp/`. The dependency fetch was the deliberate network
stage; checks are offline and locked.

- Before the fix, the new behavioral tests reproduced the refusal (four failed,
  two passed): `tmp/issue108/regression-red.{log,exit}`.
- Focused final tests: **24 capacity + 15 historical-hold + 28 state passed**,
  `tmp/issue108/focused-final.{log,exit}`.
- Structural policy and strict Clippy: **passed**, `tmp/issue108/policy-final.log`,
  `tmp/issue108/clippy.log`, `tmp/issue108/static.exit`.
- Full unchanged **`cargo +1.98.0 xtask ci`: exit 101**, in its own detached
  session, `tmp/issue108/ci-2/{log,exit}`. Structural policy, formatting, gate
  fixtures, strict Clippy and all-target/all-feature build passed. The workspace
  test stage passed the changed capacity tests, then stopped on the unchanged
  `resume_cli::dispatch_execute_admits_second_issue_beside_verified_live_worker`:
  `tests/cli/harness.rs:240`, `cooperative child did not exit`. This assertion
  waits up to **10 seconds** for the existing cooperative-worker cleanup receipt.
  The other 34 tests in that CLI binary passed. Later workspace integration
  binaries, doctests and required exact contracts were not reached by this run.
- Exact focused rerun of that unchanged timing-bounded test: **passed**, one test,
  5.10 seconds, `tmp/issue108/cli-timing-rerun.{log,exit}`:
  `cargo +1.98.0 test --offline --locked --all-features --test cli resume_cli::dispatch_execute_admits_second_issue_beside_verified_live_worker -- --exact --test-threads=1`.
  It does not call resize-capacity, and no cleanup/worker code was changed. Per
  the task instruction the full gate was not rerun repeatedly; hosted CI decides.
  No timeout, threshold or assertion was adjusted.
- The first launcher exited before establishing its session (empty log, absent
  PID, no gate result). Evidence is preserved in
  `tmp/issue108/ci/launch-failure.txt`; the confirmed detached launch above ran
  the full unmodified gate command.

Earlier development failures are preserved under `tmp/issue108/`; none was
hidden by a fallback or a changed assertion. OCR review is skipped per Andrew
2026-10-09. Hosted CI runs the same gates on Ubuntu/macOS and is required before
any merge. No merge or issue closure is part of this task.
