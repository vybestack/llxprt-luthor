# Issue #74: cancelled supervisor dispatch and audited retry

## Continuation and scope

Resumed after attempt `attempt-000932351b180ff7183d59dbe78dbe54` by inspecting
the task worktree, Git identity/status/diff, existing changes, tests and ignored
validation logs. The root HEAD was
`a580e0e7c15a2b36debf65567a754aa9fd6fde0b`, on
`luthor/task-771e168b85e96e2e87e0d55602439cdc`; the prior implementation and
regressions were uncommitted. They were preserved and completed, not replaced
from a transcript. Issue #74 was re-read with `gh`; it is open and assigned to
`acoliver`. The authenticated GitHub identity was verified as `acoliver`.
No source checkout, operator configuration/state, real worker executable or
other task worktree was edited.

## Behavior and proof boundaries

- The handshake input pipe is an explicit cancellation token checked before
  spawning the worker shim. A delayed supervisor whose coordinator has timed
  out records an atomic private `ATTEMPT.dispatch-aborted.json` marker without
  spawning a child.
- READY uses fallible writes instead of `println!`. A broken output pipe or a
  cancelled release closes the unreleased shim gate and reaps that shim before
  recording its identity in the terminal marker. Storage/identity errors are
  returned, not replaced with successful proof. No worker release is invented.
- The existing handshake deadlines are unchanged. Buffered release data takes
  precedence over hang-up: the supervisor still reads exactly `R` and verifies
  the durable release authorization. The prior attempt incorrectly rejected
  `POLLIN | POLLHUP`, intermittently aborting successful normal dispatches.
  The existing retry-refusal fixture reproduced this failure on continuation;
  it passed after the code correction, without test edits.
- Recovery requires the exact saved plan, private marker/child records,
  exclusive owner protocol, current boot continuity and independent absence of
  each recorded PID and group. Worker-release/stop intents, execution evidence,
  receipts and supervisor errors contradict this proof and keep capacity held.
- Fresh claim, worktree and absent-PR verification precede transactional audit,
  attempt completion as `dispatch_aborted`, reservation release and `attention`.
  The operator can explicitly retry the same task/session/worktree after a
  restart; the exact audited marker and quiescence are rechecked. An abort does
  not qualify for automatic natural-exit retry or historical terminal-exit
  revalidation. Existing missing-receipt behavior for workers that ran is retained.
- Old broken-pipe incidents lacking a terminal abort marker are not retroactively
  declared quiescent. Recovery fails closed without proof.

## Behavioral regressions

Five integration regressions run real built supervisors and private fake workers:

1. Delay supervisor startup until after the unchanged coordinator timeout;
   assert no child, no worker, no panic, terminal proof, recovery to attention,
   capacity release, restart and explicit retry of the same session.
2. Close READY stdout while keeping the cancellation input open; assert the
   unreleased child is reaped, no worker starts, no panic, and recovery succeeds.
3. Refuse missing/malformed/unsafe/symlinked/unknown-field/contradictory markers,
   live supervisor, wrong boot, live owner, release intent and missing owner proof.
4. Retain the reservation when PR lookup fails.
5. Refuse explicit retry when the terminal marker disappears or differs from
   the audited process identity, without reading PRs or reserving/launching.

A unit regression uses a real pipe containing `R` with its writer already
closed, verifying that buffered authorization remains readable before EOF.

## Validation environment and evidence

Rust **1.98.0**, with absolute `CARGO_HOME=$PWD/tmp/cargo-home`,
`CARGO_TARGET_DIR=$PWD/tmp/target`, and `TMPDIR=$PWD/tmp/fixtures`. Cwd stays
at the task root; fixtures use relative state paths for production Unix-socket
limits. All build/fixture/log/temporary files are confined to ignored `tmp/`.
Cargo validation is offline and locked after the prior deliberate fetch.

Focused issue regressions (5 tests), buffered-release regression, existing
retry-refusal regression, structural policy and strict workspace/all-target/
all-feature Clippy passed. Evidence is retained in `tmp/issue74/`.
The prior broad run's shell timeout and incomplete failures are preserved;
they are not counted as a pass. One detached focused launch had an absent PID,
empty log and no exit; it was preserved and relaunched in a fresh directory
with time to establish its separate session.

### Full unchanged gate and timing-only exception

`cargo +1.98.0 xtask ci` ran in a separate session and exited **101**; the
complete log and exit status are `tmp/issue74/full-ci/{log,exit}`. Policy, fmt,
xtask gate fixtures, strict Clippy and the all-target build passed. The serial
all-target test stage failed only on these three existing timing-bounded
supervisor fixtures (188 passed, 3 failed, 1 existing ignored test):

- `supervisor::pin_adoption::pin_adoption_refuses_recorded_pin_and_unfinished_running_attempt`
- `supervisor::pin_adoption::pin_adoption_refuses_wrong_hash_unsaved_path_and_repins_without_state_changes`
- `supervisor::pr_repair::active_repair_with_disappearing_pr_is_held_and_capacity_retained`

The first two failed before pin-adoption assertions, at the unchanged
`tests/supervisor/resume_fixture.rs:42` receipt-existence assertion after
200 × 20 ms polls. The third failed while constructing its initial normal-worker
fixture: the unchanged 200 × 20 ms receipt poll in
`tests/supervisor/execution.rs` elapsed, and
`tests/supervisor_support/stop_views/completion.rs:48` asserted
`fixture exit receipt missing`. None reached dispatch-abort recovery or audited
retry; these tests, their receipt publication code and their timing bounds were
not modified. All five issue regressions passed in the full run.

Each failure passed its exact focused rerun without edits (7.13 s, 4.87 s and
7.93 s, respectively), using:

```sh
cargo +1.98.0 test --workspace --all-features --offline --locked --test supervisor EXACT_TEST_NAME -- --exact --test-threads=1
```

The logs are `tmp/issue74/timing-rerun-{recorded-pin,wrong-hash,disappearing-pr}.log`.
The earlier broad supervisor check also passed (190 passed, 0 failed, 1 existing
ignored test, before the fifth issue regression was added); the final five issue
regressions, buffered-release unit regression and both retry CLI tests passed
separately after the final refactor. Following the authorized timing-only
exception, the full gate was **not** rerun. Remaining all-target suites, doc tests
and exact runtime contract probes were not reached by the failed invocation;
this is not reported as a full gate pass. Hosted CI must decide the complete
unchanged gate.

No tests, contracts, timeouts, thresholds, debt ceilings or safety rules were
lowered. Hosted CI remains required before merge. OCR review is skipped per
Andrew 2026-10-09.
