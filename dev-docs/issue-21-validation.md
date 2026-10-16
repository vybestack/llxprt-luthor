# Issue 21: audited queue capacity changes

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/21

## Scope and behavior

`resize-capacity` explicitly authorizes an existing queue's transition from an
expected persisted capacity and reservation count to the supplied config's new
capacity. Actor, reason, old/new capacity, reservation count and timestamp commit
atomically in the version-7 queue audit. Ordinary mismatch checks remain intact.
The operation does not replace the state root, release slots, rewrite task
selection/configuration snapshots, or manufacture worker-exit/ownership proof.

Fourteen behavioral integration tests cover:

- An idle 1-to-2 increase with six completed task/attempt histories, unchanged
  receipt/log/plan bytes, unchanged session/argv/PR proof, status and restart.
- Active/unreconciled launches, coordinator contention in another process,
  conflicting/missing owner locks and live registered processes.
- Reservation expectations, safe decreases, lower-bound enforcement, persisted
  capacity enforcement and stale/replayed old-limit expectations.
- Explicit execution/actor/reason, invalid limits, missing databases, unknown or
  pre-metadata versions, inconsistent identities and malformed exit proof.
- Version-6 migration, conflicting audit schemas and atomic rollback when the
  capacity update fails after the audit insert.

## Validation

Rust/cargo 1.98.0, macOS aarch64. All artifacts stayed in the task worktree's
ignored `tmp/`, with absolute `CARGO_HOME=.../tmp/cargo-home`,
`CARGO_TARGET_DIR=.../tmp/target`, and `TMPDIR=.../tmp/f`. Existing confined-relative
Unix-socket fixtures were used unchanged.

Passed:

- `cargo +1.98.0 test --offline --locked --test capacity_resize --test state -- --test-threads=1`
  (14 new capacity tests and 28 state tests).
- The exact daemon historical-upgrade test after correcting its synthetic v5
  schema fixture (one test).
- Strict all-target/all-feature Clippy, including the repository's cognitive and
  type-complexity denies.
- **Full unchanged `cargo +1.98.0 xtask ci`: exit 0**, including policy/contracts,
  formatting, xtask tests, strict Clippy, build, the serialized workspace suite,
  and every required exact contract rerun.
- `git diff HEAD --check`; final status/diff reviewed against the task base.

The initial full run exposed one additional daemon fixture that declared schema
v5 after constructing the current schema, but left the new audit table behind.
It now drops both post-v5 tables when constructing the old schema; its behavior
and assertions are unchanged. The focused rerun and subsequent full gate passed.
The original failure is preserved in `tmp/issue21-ci/log`; the final full result
is in `tmp/issue21-ci-2/{log,exit}`. No migration fallback, timeout, assertion,
threshold, debt ceiling, contract, required check or CI definition was relaxed.

OCR review skipped per Andrew 2026-10-09. Hosted CI runs the same gates and must
succeed before any merge; no merge is part of this task.
