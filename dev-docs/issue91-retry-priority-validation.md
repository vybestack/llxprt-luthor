# Issue 91: automatic retry queue priority

## Verified scope

Re-read tracker issue [91](https://github.com/vybestack/llxprt-luthor/issues/91),
its repository identity and the current scheduling/retry paths before editing.
GitHub identity was `acoliver`, the issue was open and assigned to `acoliver`,
and the task branch was
`luthor/task-fc338ba8ba3818f1fd37e3215cc8512e`. Initial HEAD was
`4e19066519455d3972be63de37a8731ac96ccf6b`, matching the mapped base
`work/luthor-issue-to-pr-daemon`; initial tracked/untracked status was clean.

The original scheduler already attempted expired retries before candidates,
but selected fresh candidates during backoff. The new capacity-one regression
failed against that implementation because a new issue launched during backoff
(`tmp/issue91/regression-before.log`).

## Scheduling contract

This uses logical queue priority, not a retained durable reservation. New
candidate selection is deferred while any automatic task retry is in `backoff`,
`capacity_full`, or `worktree_busy`. This intentionally allows idle capacity
while waiting, including with capacity greater than one. Retries are evaluated
in original task-selection order before candidate dispatch in every execute
cycle. Already selected source recovery and explicit operator requests retain
their existing paths. No worker is preempted and no capacity check is bypassed.

Pending retry observations include `backoff_until_unix_secs`: the durable
outcome-observation time plus the current policy's backoff. It stays visible
when the deadline has passed but capacity or worktree ownership is unavailable.
Non-pending observations omit it. Deadline arithmetic is checked and errors
propagate; no fallback changes retry authorization.

Exhaustion, disabled policy/classes, non-transient/unclassified/human-input
outcomes, ineligible retry contexts and task-local admission refusals do not
block independent new selection. Existing uncertain-dispatch, identity, claim,
PR absence, receipt, worktree ownership and process checks are unchanged.

## Regression coverage

Four new behavioral tests use the existing real natural-exit worker fixture:

- Capacity one, typed turn-budget failure, ready new candidate: repeated polls
  and restart preserve backoff/deadline without creating a task, attempt or
  reservation; after backoff the original retry gets the slot first.
- Exhausted, class-disabled, policy-disabled, non-transient, unclassified,
  human-input and ineligible tasks allow the ready candidate to launch.
- A retry waiting on occupied capacity reports task/attempt and its expired
  backoff deadline, then launches before the new candidate after release.
- A busy worktree owner preserves priority without consuming an attempt or
  reservation; releasing it allows the retry to win the next slot.

## Validation environment and evidence

Rust **1.98.0**, repository-root cwd, and absolute confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

Dependencies were deliberately fetched once with `cargo +1.98.0 fetch --locked`.
Existing relative fixture state roots preserve Unix-socket path limits. All
builds, Cargo caches, fixtures and logs are under this worktree's ignored `tmp/`.
No private operator configuration/state, production worker executable, source
checkout or other task worktree was changed.

- `cargo +1.98.0 xtask policy`: passed (`tmp/issue91/policy.log`).
- Focused new regressions: **4 passed** (`tmp/issue91/priority.log`).
- All automatic-retry regressions: **18 passed**
  (`tmp/issue91/automatic-retry.log`).
- An additional coordinator/daemon test invocation was interrupted at the
  shell tool's 600-second limit during coordinator tests, with no reported test
  failures before interruption. This is incomplete, not a pass; its output is
  preserved in `tmp/issue91/coordinator-daemon.log`. An initial invocation also
  used a nonexistent standalone `daemon_completion` target; the correct target
  is `daemon`. The unchanged full gate includes both targets.
- Full unchanged `cargo +1.98.0 xtask ci`: **passed, exit 0**, including
  structural policy, formatting, gate fixtures, strict Clippy, workspace build,
  all-target serial tests, documentation tests and every required exact
  contract. Output and exit status are in `tmp/issue91/ci-run2/`. The first
  detached launch exited before writing any log or exit file; its PID was
  confirmed absent and its empty output was preserved in `tmp/issue91/ci/`
  before relaunch.

No test, assertion, timeout, lint threshold, contract, debt ceiling or gate was
weakened. OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same
required gates and must succeed before any merge.
