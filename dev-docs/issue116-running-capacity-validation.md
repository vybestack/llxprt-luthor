# Issue 116: increase capacity beside confirmed running attempts

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/116

## Verified requirements and safety boundary

Issue 116 belongs to `vybestack/llxprt-luthor` and is assigned to `acoliver`.
The task began at `9646eee` on the required task branch with a clean worktree;
the mapped PR base is `main`. GitHub authentication was verified as `acoliver`
before writes. No private queue/configuration, worker executable, source checkout
or other task worktree was modified.

An **increase only** can now count confirmed running attempts. Read-only checks
reuse the existing reconciliation plan/process evidence readers and live-process
identity verification: exact launch/dispatch, selection, worktree, claim,
registered child, supervisor, release and sent gate must agree. The held task
must retain its exact reservation and unknown outcome. Missing stages, duplicate
or misbound stages, pending stops, terminal evidence, receipts, supervisor errors
and invalid live-process identities cannot authorize the exception.

The owner protocol must match the existing private lock's task/attempt, inode
and nonce; the worker's lock must still be busy. Observation does not replace or
recreate the lock. Registrations outside that exact running attempt still require
ordinary process/group absence. All other tasks retain the original absence and
exclusive-owner checks. The coordinator remains held from read-only observation
through the immediate transaction, so no cooperative reservation or launch writer
can intervene. Worker exit alone does not alter its reservation.

The verified reservation count, expected old limit and lower bound remain
mandatory. All decreases retain the previous behavior, including refusal with a
running attempt even when the proposed limit covers its reservation. The #108
permanent-hold classifier is unchanged and can coexist with a confirmed worker.
No reservation acquisition/release, daemon launch, receipt reconciliation,
continuation or retry authorization path was changed.

Each audit and CLI result names counted running task/attempt IDs in stable order
in `running_attempts`, separately from `permanent_holds`. Schema version 9 adds
this JSON array with `[]` for previous audits, preserving their existing values.
The audit and capacity update remain atomic; schema conflicts and upstream
SQL/I/O/serialization errors are errors, not adoption or successful classification.

## Behavioral regressions

Eight new real-worker CLI tests cover:

- Successful increase, named durable audit, unchanged task/attempt/reservation/
  launch/preparation/evidence history and owner bytes, and no GitHub invocation.
- A confirmed worker beside a permanent historical hold, with separate named
  audit lists and both reservations retained.
- Refusal of decreases and stale capacity/reservation expectations.
- In-progress dispatch (missing gate send/release), contradictory dispatch and
  pending stop; exact original fixture evidence is restored only for shutdown.
- Finished worker with an unreconciled receipt, independent of stop intent.
- Conflicting owner nonce, missing owner, contradictory child/supervisor proof,
  duplicate gate-send evidence and live registrations outside the counted attempt.
- Atomic audit/limit rollback on a database write failure.

Two new migration tests preserve all v8 audit fields (including historical holds)
and verify rollback/refusal of conflicting v8 schemas. Existing schema assertions
advance to version 9 and the unsupported-next-version test advances to 10; no
assertion, timeout, threshold, ceiling, contract or required check was weakened.
Existing historical-hold, capacity and state tests remain intact.

## Validation

Rust **1.98.0**, macOS aarch64, stable task-worktree cwd:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
cargo +1.98.0 fetch --locked
cargo +1.98.0 test --offline --locked --test cli cli::capacity:: -- --test-threads=1
cargo +1.98.0 test --offline --locked --test capacity_resize --test historical_hold --test state -- --test-threads=1
cargo +1.98.0 xtask policy
cargo +1.98.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity -D clippy::type_complexity
cargo +1.98.0 xtask ci
```

Dependencies, builds, logs and temporary files stay under this worktree's ignored
`tmp/`. The fixture state/socket paths remain confined-relative, following
`xtask/README.md`; production socket limits and fixture deadlines are unchanged.

- The new success regression failed against the original implementation with
  `active or unreconciled launch; reconcile first`:
  `tmp/issue116/regression-red.{log,exit}`. Early fixture-shutdown failures are
  also preserved there and in `focused-initial.*`; shutdown now restores exact
  original evidence before requesting the ordinary identity-safe stop.
- New real-worker tests: **8 passed**, `tmp/issue116/running-focused-final.*`.
- Capacity, historical-hold and state suites: **26 + 15 + 28 passed**,
  `tmp/issue116/focused-final.*`.
- Structural policy and strict Clippy: **passed**,
  `tmp/issue116/policy-final.*`, `tmp/issue116/clippy-final.*`.
- Full unchanged **`cargo +1.98.0 xtask ci`: exit 101**, in a detached session,
  `tmp/issue116/ci/{log,exit,pid}`. Structural policy, formatting, gate fixtures,
  strict Clippy and all-target/all-feature build passed. Workspace tests passed
  180 library tests and the capacity suite (26); the CLI binary stopped with
  **43 passed, 2 failed**. Later workspace binaries, doctests and required exact
  contracts were not reached by this run.
- Both failures occurred in the new regression cases' **unchanged shared fixture
  setup, before any resize call**: `tests/cli/harness.rs:230`, `worker A did not
  start`, bounded by its existing **5-second** startup wait. The affected cases
  were `cli::capacity::finished_worker_with_unreconciled_receipt_cannot_be_counted_as_running`
  and `cli::capacity::in_progress_and_unreconciled_launches_still_refuse_increases`.
  This is a setup/launch timing failure, not an accepted failure of resize logic.
- Exact all-feature focused rerun of the in-progress/unreconciled case: **passed**,
  20.13 seconds, `tmp/issue116/launch-timing-rerun.*`.
- The finished-worker case's first exact all-feature rerun also failed in setup,
  this time in the existing **5-second READY handshake**, before any resize:
  `tmp/issue116/finished-worker-timing-rerun.*`. Its second exact rerun **passed**,
  4.28 seconds, `tmp/issue116/finished-worker-timing-rerun-2.*`. Both invocations
  and their real results are retained; there is no retry inside the tests.
- No deadline, threshold or assertion was changed. The full gate was not rerun
  repeatedly for these pre-resize fixture timing failures; hosted CI decides.

OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same unchanged
gates on Ubuntu/macOS and is required before any merge. No merge or issue closure
is part of this task.
