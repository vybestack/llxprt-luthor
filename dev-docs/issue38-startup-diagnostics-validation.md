# Issue #38: diagnose blocked startup reconciliation

Verified tracker: `vybestack/llxprt-luthor#38`, assigned to `acoliver`.
The task started at `d6670f1db85c6304ed53485f53e862c02b7401fc` on
`luthor/task-99bc32cc97674e9222a3eab8163e8e68` with a clean worktree.

## Behavior

A blocked execute cycle now prints the existing `mode` and `blocked` fields
plus `reconciliation` and `source_holds`, using the same narrow diagnostic
projection as the normal execute report:

```json
{"mode":"execute","blocked":true,"reconciliation":[{"task_id":"task-example","attempt_id":"attempt-example","status":"error","error":"launch plan does not match verified task or worktree"}],"source_holds":[{"task_id":"task-example","kind":"stop"}]}
```

Every reviewed attempt retains its identity and status. Error reviews include
their existing error text; non-error reviews do not acquire a fabricated error.
Source holds contain only task identity and kind. The projection does not dump
configuration, launch plans, receipts, source-intent detail or evidence payloads.
It does not change the blocking predicate, release reservations, schedule work,
suppress errors or add retries. `--once` still exits 1 with the existing stderr
message; the existing continuous-mode polling policy is unchanged.

## Behavioral regressions

- `daemon_startup::blocked_startup_reports_all_attempt_errors_and_source_hold_kinds`:
  real CLI, two inconsistent durable attempt identities producing real
  `AttemptReview::Error` entries, and an unresolved stop. Asserts the complete
  attempt diagnostics and source-hold shape, exit 1, retained reservations and
  pending attempts, no discovery/assignment/launch, and no private intent detail
  in stdout.
- `daemon_startup::source_stop_alone_reports_the_barrier_without_fabricating_an_attempt_error`:
  a stop blocks even when the attempt review is only held. Asserts source kind,
  held status without an error, exit 1, retained reservation and no dispatch.
- `daemon_completion::active_finish::daemon_retries_legacy_pause_pr_verification_after_transient_read_failure`:
  real supervised worker and hermetic GitHub/SSH fixtures reproduce the older
  stopped-after-PR hold. After a successful open-PR lookup, an HTTP 500 during
  authenticated-identity verification leaves it held with no completion proof
  and still pending. Restoring only the upstream read lets the next daemon
  execute cycle reach `pr_complete`, release capacity and persist exactly one
  verified PR proof. No manual reconcile or local-state repair is used.

The recovery test specifically covers a transient **verification** read after
an open lookup, not all possible failures of the initial PR search. Initial
fixture development injected a PR-detail lookup failure instead, producing
`pause PR read failed`; the fixture was corrected to target the verification
stage described above. Existing assertions, timeouts and production recovery
rules were not relaxed. Development logs are retained under `tmp/issue38/`.

## Validation

Rust **1.98.0**, with stable worktree cwd and absolute confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
cargo +1.98.0 fetch --locked
cargo +1.98.0 test --offline --locked --test daemon -- --test-threads=1
cargo +1.98.0 xtask ci
```

Fixture state roots remain relative to the stable worktree cwd, following
`xtask/README.md` Unix-socket guidance. Builds, dependencies, fixtures and raw
logs remain under ignored `tmp/`. No gate, contract, lint/debt ceiling, timeout,
threshold or existing assertion was changed.

- Focused startup regressions: **2 passed** (`focused-startup.log`).
- Full daemon integration target: **21 passed**, 186.16s
  (`focused-daemon.log`), including all three new regressions.
- **Full unchanged `cargo +1.98.0 xtask ci`: PASS (exit 0)**. Includes
  structural/coupling/suppression policy, formatting, gate fixtures, strict
  Clippy, workspace/all-target/all-feature build and serial tests, doctests,
  and every exact required behavioral contract. Log:
  `tmp/issue38/ci-run2/log`; exit file: `tmp/issue38/ci-run2/exit`.
- `git diff --check`: pass.

The first detached gate launch left an empty log, no exit file and no live PID;
evidence is retained in `tmp/issue38/ci/`. The required detached session pattern
was relaunched with a startup wait in `tmp/issue38/ci-run2/`.

OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same unchanged
gates and is required before any merge.
