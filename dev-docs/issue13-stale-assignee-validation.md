# Issue 13: stale-assignee completion fixture

## Scope and verified mapping

Issue `vybestack/llxprt-luthor#13` was open and assigned to `acoliver` before edits. Its body and comment were read with the GitHub CLI. SSH authentication and the CLI API identity both reported `acoliver`. The mapped base `work/luthor-issue-to-pr-daemon` and the initially clean task branch both pointed at `1b63a8756d77a9a7fb584775ecfadf49a9fb4a8e`.

This is a test/fixture change only. Production completion, identity, PR verification, reservation, and retry rules are unchanged. No workflow, contract, debt ceiling, lint threshold, or test ignore is changed.

## Diagnosis and bounded red reproduction

`dispatched_fixture` previously waited only for the receipt pathname. Receipt publication is not a process teardown barrier. `reconcile_with_pr` first calls `reconcile_attempt`; any non-Completed result returns before natural/stopped completion-claim checks. Examples include live tracked descendants, invalid receipts, and log-drain failures. These early `Held` results carry a reason but do not record a completion `held_reason`, reconcile the exit, release capacity, or inspect the PR/assignee. A duplicate durable supervisor identity instead fails closed with `State(LaunchBlocked)`.

A controlled reproduction added the test process's real observed identity as a `tracked_descendant` to the original natural-exit test. The unchanged broad `Held` assertion passed, followed by the reported `Option::unwrap()` failure on missing `held_reason`. The red invocation was:

```sh
cargo test --offline --locked --test supervisor \
  natural_exit_stale_assignee_is_held_despite_matching_open_pr -- --exact
```

It exited 101; evidence is in ignored `tmp/issue13-controlled-red.log`. The temporary injection was removed. This demonstrates the competing-result problem; it does **not** identify which early hold caused the historical run or establish a deterministic production bug.

The initial isolated unmodified test passed. A bounded baseline repetition also encountered `ReadyTimeout` during fixture launch (run 7), rather than the issue's missing-reason failure. An early concurrent validation attempt encountered a missing receipt. These are retained in `tmp/issue13-baseline-repeat.log` and `tmp/issue13-repeat.log`; they are not reported as successful validation. A first serial repetition also encountered `ReadyTimeout` in round 13 (`tmp/issue13-repeat-final.log`). Launch-failure diagnostics were added, without retrying a failed launch or changing its production timeout. The subsequent final serial repetition passed all 20 rounds (`tmp/issue13-repeat-diagnostic.log`, exit 0). No failure is relabeled as a pass; readiness failures are distinct from the reproduced missing-reason assertion.

## Fix and regressions

Both existing stale-assignee contracts now use a dedicated completion fixture. It launches in the workspace-local temporary directory, detects a supervisor error receipt while waiting for the exit receipt with a deadline, and waits for the supervisor and child process/group to be absent using the existing bounded fixture helper. It asserts the reservation is still retained and no completion reason/exit has been synthesized. It does not retry reconciliation or whitelist unexpected identity failures. A group cleanup guard protects teardown on fixture failure.

The existing assertions remain, including the recorded completion-claim reason, held phase, no verified PR, exact durable exit outcome, released reservation after proven exit, blocked dispatch capacity, and one PR/claim recheck. The returned result is now required to be exactly `Held("completion claim changed")`, not just any `Held`.

Behavioral regressions cover:

- a real live tracked child: early hold retains capacity and performs no completion checks; after kill/reap, reconciliation records the stale-assignee hold and durable exit without accepting the matching PR;
- an invalid receipt identity: early hold with retained capacity and no completion checks;
- a recorded log failure: early hold with retained capacity and no completion checks;
- contradictory duplicate supervisor evidence: a fail-closed error, with reservation retained and no completion/PR acceptance.

Test support lives beneath `supervisor_support/stop_views`, alongside the existing stopped/natural-exit fixture support. The receipt-ready shared `dispatched_fixture` also uses the same process/group teardown barrier. A final-file-state full gate exposed the same competing early-hold behavior in `natural_exit_pr_error_keeps_held_slot_and_evidence` (reservation 1 instead of 0); that failed run is retained in `tmp/issue13-ci-final.log`. The exact competing test passed after installing the shared barrier. The two existing contract names and their assertions remain in place. This keeps the unchanged structural ratchet satisfied without changing debt or policy.

## Validation environment

All commands run from the assigned task worktree with absolute confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/fixtures"
```

The cwd remains stable so relative fixture state paths fit Unix socket limits. Dependencies were deliberately fetched with `cargo fetch --locked`; subsequent checks use the repository's offline/locked commands and Rust 1.98.0. Final validation results are recorded in the PR body.

## Results

- Focused stale-assignee tests: passed (3 tests, including the transition regression).
- Focused completion regressions: passed (4 tests).
- Diagnostic serial repetition: 20 rounds of both focused groups passed, 140 test executions total (the transition regression runs in both groups); exit 0. After the shared barrier change, the final repetition additionally includes the exact competing PR-error test: all 20 rounds passed, 160 executions, exit 0 (`tmp/issue13-repeat-shared.log`).
- First complete unchanged `cargo xtask ci`: exit 0, including workspace tests and required exact contracts. The later failed competing-outcome run is not hidden; the final shared-barrier gate passed with exit 0 and is recorded in `tmp/issue13-ci-verified.log` (the preceding shared run caught a needless-borrow lint in the extracted helper; fixed without suppression).
- Ubuntu and macOS hosted quality results are recorded in the PR once available.
