# Issue #75: wait for a failed job's workflow run

## Behavior and safety

A required Actions job can fail while another job in its workflow is still
running. The rerun reader now distinguishes a verified `queued` or `in_progress`
workflow from an invalid run. It returns pending without POST or rerun evidence;
the task remains `pr_complete`, with no repair worker or hold, and the next
cycle reads the run again. A completed run can be rerun exactly once for that
head. Run id, head, repository and authenticated `acoliver` checks still precede
any pending result or write. Unknown/missing statuses remain malformed.

The reader invokes a fallible intent callback only after validating a completed
run and immediately before POST. Callback/storage failures prevent POST and
propagate unchanged. `pr_failed_jobs_rerun_intent` is the durable write-ahead
record; `pr_failed_jobs_rerun` and its acknowledgement are recorded only after
GitHub accepts POST. An interrupted or ambiguous POST is never retried or used
to authorize code repair. History exposes the separate intent. Historical
rerun-plus-acknowledgement records retain their existing interpretation;
unacknowledged historical records still hold. No state migration or automatic
release of previously held tasks is attempted.

## Regressions

- Reader tests cover queued/in-progress workflows without intent or POST,
  identity/id/head/repository/status validation, intent-before-POST ordering,
  failed intent preventing POST, and a rejected POST not reporting acceptance.
- Real `GhPullRequestReader` supervisor fixtures cover a failed required check
  alongside a pending release job, repeated cycles and a store restart, followed
  by workflow completion at the same run attempt and exactly one accepted POST.
  They assert `pr_complete`, no worker, no new hold or rerun evidence while
  pending, and exact accepted rerun evidence after completion.
- Further fixtures preserve malformed-head holds even on pending runs, distinguish
  rejected POST intent from successful rerun evidence, and refuse both new and
  historical unacknowledged rerun intents across restart.
- Existing configured-check selection, rerun-once, repeated-failure repair and
  flaky-on-rerun behavior remain covered without changing their assertions.

## Validation

Rust **1.98.0** (`88d9e12ae`), stable worktree cwd and absolute confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

The existing fixture helpers retain relative state paths for production
Unix-socket limits. Dependencies were fetched deliberately with
`cargo +1.98.0 fetch --locked`; validation is offline and locked. Logs and exit
statuses are under ignored `tmp/issue75/`. No gates, required contracts, debt
ceilings, lint settings, timeouts or assertions were weakened.

Focused reader tests (4), issue75 supervisor regressions (4), existing configured
check regressions (5), and structural policy pass. During development, a private
field access in the new fixture was corrected to use its own SQLite connection;
the new waiting assertion was corrected to preserve historical fixture hold
reasons rather than assuming none existed; and the pending-release fixture was
configured to select both named gates. These were test setup errors, not relaxed
production checks.

The unchanged full **`cargo +1.98.0 xtask ci` passed (exit 0)** in a detached
session: policy, fmt, xtask fixtures, strict Clippy, locked build, workspace
all-target serial tests, documentation tests, and every exact required contract.
Evidence: `tmp/issue75/full-ci/{log,exit}`. There were no timing-test failures or
focused reruns of full-gate failures. Existing opt-in ignored tests remain opt-in.

The first detached launcher disappeared before writing any gate output or exit
file. Its absent PID and empty log were preserved under
`tmp/issue75/full-ci-launch1/`; a fresh launch was allowed to establish its new
session before the shell call returned. The verified second launch completed the
entire gate successfully without changing any commands or limits.

OCR review is skipped per Andrew 2026-10-09. Hosted CI is required before merge.
