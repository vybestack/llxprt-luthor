# Issue #63: let a worker finish after publishing its own PR

A worker normally publishes its PR before writing its final summary and exiting.
The active monitor previously treated that PR as a reason to request a stop,
losing the summary and potentially leaving a `pause PR present` hold that
blocked the capacity-one queue.

## Behavior

For an owned issue claim, the active monitor verifies the PR's repository,
branch, allowed author, exact tracker/closing references and SHA equal to the
live local task tip. It records an attempt-scoped `active_open_pr` observation
and returns `Running`, without a stop request or completion transition. The
reservation stays occupied until the worker exits under its existing deadline.
The launch snapshot may precede worker commits, so the tip comparison inspects
the owned worktree's current identity rather than its immutable dispatch SHA.

Exit reconciliation still accounts for the receipt and process identities first,
then independently reads and verifies the PR and issue claim before committing
`pr_complete`. Active observations are not reused as terminal proof. Existing
repair completion retains its saved-PR and exact-tip state-commit error path.
Conflicting branches/tips, repositories, authors or multiple candidate PRs stay
held; lost claims retain the existing stop behavior.

Startup also revisits the latest held attempt whose latest pause lookup reported
an open PR. This restores legacy `pause PR present` tasks through normal receipt,
claim and fresh PR verification, not by trusting their old lookup or overriding
other process-safety checks. A successful completion is not revisited on the next
cycle, and capacity becomes available again.

## Behavioral regressions

The real daemon CLI fixture uses supervised workers, a local bare remote and an
isolated SSH fixture. A worker commits/pushes its task branch, publishes the PR,
then sleeps/waits across two daemon cycles before printing its final JSON summary
and exiting 0. Tests require no stop intent or receipt stop signal, retained
capacity and no completion while active, followed by exact-tip `pr_complete`
with released capacity and idempotent subsequent cycles.

Additional cases reconstruct the legacy stopped/held state and require fresh
completion on startup. Active and legacy restoration conflict cases cover wrong
branch, tip, author, head repository, closing reference and ambiguous PRs. They
require holds and forbid completion proof. A supervisor regression independently
counts fresh claim/PR reads after natural exit and checks the preserved summary.
Existing completion fixtures now provide real local SHAs and closing references
instead of placeholder matching evidence; their safety assertions are unchanged.

The main active-worker and restoration regressions both failed against the base
production paths and passed after the fix. Negative-control logs are retained in
ignored `tmp/issue63/baseline-active.log` and `baseline-restore.log`.

## Validation

Rust **1.98.0 (88d9e12ae 2026-08-18)** on macOS, stable worktree cwd, absolute
worktree-confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export RUSTUP_TOOLCHAIN=1.98.0
```

Relative fixture state roots preserve production Unix-socket limits. No gate,
contract, timeout, assertion, lint threshold or structural ceiling was weakened.

Passing focused validation:

- Daemon target: 14 passed, including all four new behavioral regressions.
- Supervisor live cases: 11 passed; stopped-exit: 2; natural-exit: 8.
- Configured PR checks: all 5 passed on continuation's focused rerun.
- Continuation PR completion: all 6 passed, including future-template and
  forged/duplicate/wrong-receipt/unauthorized-replay cases.
- Amended missing-receipt recovery: passed, preserving no-write refusal.
- Structural policy, formatting and strict workspace/all-target/all-feature
  Clippy passed.

The first continuation full gate reached coordinator tests and failed four
completion cases because their shared fixture still supplied `abc123` and no
closing reference. That fixture was corrected to the real local tip and exact
references, without changing assertions; the focused group then passed. Logs:
`tmp/issue63-resume/log`, `amended-completion.log`, `amended-recovery.log`.

An earlier broad supervisor focused call was killed by its 600-second shell-call
limit before completion; it is not a gate result. Two configured-check tests had
reported failures in that incomplete run; the entire five-test group passed on
focused rerun. Logs: `tmp/issue63-resume/supervisor.log`, `configured-checks.log`.
The previous attempt's plain-nohup gate was killed without an exit marker; its
evidence is preserved under `tmp/issue63/`. Continuation uses a detached session
and an explicit exit marker, keeping the full gate and all its bounds unchanged.

**Final unchanged `cargo +1.98.0 xtask ci`: exit 101**, with exactly one
unrelated timing-bounded failure. The supervisor target passed 162 tests, with
one existing helper ignored, and failed
`supervisor::pr_repair::missing_or_replaced_pr_after_repair_stays_held_without_normal_retry`
at `tests/supervisor_support/stop_views/completion.rs:48`:
`fixture exit receipt missing`. Its initial dispatched fixture waits at most
200 iterations of 20 ms before requiring a receipt, before any PR reconciliation
affected by this change. No wait, threshold or assertion was changed.

The unchanged exact all-feature focused rerun passed (20.82 seconds, all four
scenarios). Evidence: `tmp/issue63-resume/ci2/log`, `ci2/exit` and
`repair-timing-rerun.log`. The full run started at 2026-10-13 22:46:16 UTC;
policy, formatting, scanner fixtures, strict Clippy and build passed, as did the
coordinator and daemon targets. Per the authorized unrelated-timing-test
exception, the full gate was not rerun repeatedly. Hosted CI must decide the
final full-gate result.

Doctests and all exact required contracts are also run separately because the
full gate stopped before those stages; both passed (combined exit 0). Evidence:
`tmp/issue63-resume/remaining/log`, `remaining/exit`.
OCR review is skipped per Andrew 2026-10-09. Hosted Ubuntu/macOS CI runs the same
unchanged gates and remains required before any merge.
