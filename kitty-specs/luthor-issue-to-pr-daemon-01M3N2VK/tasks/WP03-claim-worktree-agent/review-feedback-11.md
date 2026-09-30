# WP03 review findings

Reviewed source commit: `314b3c319481784d0ab67b3953f2ab26bec472b7` on `work/luthor-issue-to-pr-daemon`.

## Blocking: a failed log-failure evidence write abandons a live worker

`src/supervisor.rs:1202-1210` calls `record_log_failure(...)?` before `stop_failed_log_child(...)`. If a stream write or sync fails while the state database is also unavailable or full, `record_log_failure` returns an error and the supervisor exits without attempting to stop its still-running worker group. The child handle is dropped without killing the process. The reserved slot remains held, but the worker may keep executing without a supervisor or functioning log capture. This violates WP03's log-failure stop requirement and the architecture's rule to stop an agent when telemetry fails.

Ensure the failure path accounts for the worker even when persisting the stop intent or `log_failure` evidence fails. Keep the reservation held until the group is independently proved absent; do not manufacture a clean receipt or silently release capacity. Add a fake-child fault test in `tests/supervisor.rs` that makes both the log writer and the evidence write fail while a worker stays alive without writing output. Assert that no unobserved worker is left running, and that the slot is not released without group-termination proof. The existing `live_worker_log_write_failure_stops_and_holds_both_streams` test exercises only a writable state store.

## Verification stability

`cargo test --offline --locked --all-targets` failed on the first run at `tests/supervisor.rs:606` (`detached_same_binary_dispatch_records_gate_and_worker_receipt`: `marker.exists()`), while focused supervisor tests and the repeated full suite passed. The test polls the receipt for only 100 × 30 ms and then asserts worker startup, so investigate whether it is an execution race or a short test deadline under concurrent load. Retain this failure in the review record; make the fake-child check reliably reflect the actual launch/receipt invariant.

The `WP04` dependency must rebase or wait for WP03 changes. `src/supervisor.rs` and `tests/supervisor.rs` are shared mission-lane files; coordinate the fix with other work packages before changing them.

## Review checklist

1. Dead code: PASS; production entrypoints are wired through `main.rs`, `daemon.rs`, `coordinator.rs`, `supervisor.rs`, and `cli.rs`.
2. Synthetic-fixture test: PASS for reviewed WP03 integration coverage; the fake adapters and worker invoke production claim, scheduling, launch and reconciliation.
3. Silent empty return: PASS; no unexamined silent-empty failure path found in the reviewed flow.
4. FR coverage: PASS for WP03 claim, worktree, agent, supervision and local-control behaviors, subject to the log-failure gap above.
5. Frozen surface: N/A; no frozen file is identified in the WP specification or mission contracts; no `contracts/` artifact exists.
6. Locked decision: PASS for the reviewed paths; no automatic assignment retry, unassignment or PR-from-exit completion found.
7. Shared-file ownership: PASS with the coordination note above for the shared lane.
8. Production fragility: FAIL; propagating the evidence-write error at `src/supervisor.rs:1205` skips termination of an active child.

Verification used `CARGO_TARGET_DIR=/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/wp03-root-target`: format check PASS; focused `claim`, `worktree`, `supervisor`, `coordinator`, `cli`, `daemon` tests PASS; all-target tests FAIL on first run, PASS on second; strict Clippy `--all-targets -- -D warnings` PASS. Logs: `tmp/wp03-independent-checks/`.
