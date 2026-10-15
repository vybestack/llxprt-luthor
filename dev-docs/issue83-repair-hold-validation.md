# Issue 83: specific, bounded PR-repair holds

Verified issue: https://github.com/vybestack/llxprt-luthor/issues/83
Repository: `vybestack/llxprt-luthor`; mapped base:
`work/luthor-issue-to-pr-daemon`. Initial task HEAD:
`4672529ab150539d9a4f2a89520e2f0950eb4aac`; initial status clean.
Authenticated GitHub account verified as `acoliver` before writes.

## Requirements and implementation

The issue's corrected report says restoration worked; the remaining bug is
loss of the failed check's identity. PR identity, owner acquisition/protocol and
exit revalidation used to collapse into `SupervisorError::Conflict`, then the
coordinator mapped all supervisor failures to one generic hold.

- Preserve typed check causes through the existing reconciliation path; do not
  infer a cause from upstream strings or relax any proof.
- Record the same bounded reason in `held_reason` and cycle `pr_repair.held`.
  Exhaustively classify every supervisor error variant without its payload.
- Busy-owner and verified-running-attempt holds explain that restore will retry
  next cycle. Retained restoration reports preserve the current bounded cause.
- Recognize the new exact identity-hold reasons in restoration alongside the
  historical reasons. All accounting, receipt, ownership, session, claim, PR and
  worktree checks remain mandatory; launch/build/session/infrastructure codes
  are not restoration candidates.
- Keep explicit legacy worker-pin adoption usable after its new bounded repair
  hold. The existing legacy-plan and quiescence proofs remain unchanged.

## Hermetic regression coverage

- Every supervisor error class has a distinct bounded code; deliberately secret
  IO/SQL/config/JSON/worktree/build payloads cannot enter the repair reason.
- Receipt missing/invalid, process evidence missing, launch-plan evidence missing,
  owner unavailable, owner protocol invalid, PR identity changed and health-head
  mismatch identify the check, persist/report the same reason and do not launch.
- A busy owner reports retry guidance on the repair and retained-restore paths;
  release plus restart permits proof-based restoration and a single repair.
- A missing receipt stays held until the original private receipt verifies again.
- New reasons cannot bypass live reservations, unfinished attempts, semantic
  conflict or incompatible-session guards.
- Legacy worker-build holds never persist the upstream payload before the bounded
  repair hold; explicit pin adoption still works.
- A whole daemon execute cycle persists and reports the PR identity cause.

## Validation environment

Rust **1.98.0** with absolute worktree-local paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

Cwd stays at the task root; existing fixtures use relative state roots to retain
production Unix-socket limits. Dependencies were deliberately fetched with
`cargo +1.98.0 fetch --locked`; checks use the locked offline cache. Logs are in
ignored `tmp/issue83/`. No gate, contract, timeout or lint threshold was changed.

Focused validation: all-target check, structural policy, strict workspace
Clippy, supervisor classification unit tests, daemon-cycle regression and the
six new behavioral diagnostics tests passed. The first broad PR-repair run
passed 57 tests and exposed two mistakes in the new test setup: recreated
receipt permissions were not private, and health mismatch was incorrectly
expected to block identity-only restoration. Those test setups were corrected;
the focused regressions pass. Existing legacy-pin adoption and historical
restoration tests passed in that broad run.

The unchanged full **`cargo +1.98.0 xtask ci` passed (exit 0)** in its detached
session. Structural policy, formatting, gate fixtures, strict Clippy, all-target
build, serial workspace/all-target tests, doc tests and every required behavioral
contract passed. No timing-failure exception or focused full-gate failure rerun
was needed. The completed log and exit file are in `tmp/issue83/ci2/`.

OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same gates and is
required before merge; this task does not merge the PR.
