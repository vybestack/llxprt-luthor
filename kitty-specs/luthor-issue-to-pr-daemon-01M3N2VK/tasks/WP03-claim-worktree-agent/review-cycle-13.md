---
affected_files: []
cycle_number: 13
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T18:08:25Z'
reviewer_agent: user
wp_id: WP03
---

# WP03 review feedback

Reviewed implementation: `c606dc977948c2b8a1063e8a5b6b1e6bb6c3b866` (lane); the root checkout at review had identical `src/` implementation. WP04 PR-completion proof was excluded.

## Blocking findings

1. **A pause racing a natural exit strands an accounted attempt.** In `src/supervisor.rs:718`, `request_stop` commits a stop intent before checking whether the worker has already exited. If a short-lived worker writes its normal receipt immediately before `pause TASK` (or exits during stop verification), the receipt has `stop_signals: []`. `reconcile_attempt` can verify the receipt and release the reservation, but `src/state.rs:1079-1102` refuses the paused path because no stop signal was sent, while `src/state.rs:1105-1137` refuses the natural-exit path because the stop intent exists. `src/coordinator.rs:233-250` therefore leaves the task held after a verified exit and absent PR, and `resume_context` at `src/state.rs:167-229` cannot resume it. Reproduce with the existing `running_worker`/`stopped_receipt` integration fixture in `tests/supervisor.rs`: let a worker exit without a signal and wait for its receipt; issue `request_stop` before reconciliation; perform `reconcile_with_pr` with an exhaustively absent fake PR result. Assert the attempt is accounted, no worker is signaled, and the task becomes `attention` (natural exit) or has a documented safe operator resolution. Add an equivalent test for exit between durable stop intent and signal. Decide the final state from the actual receipt and verified group absence, without manufacturing a signal or allowing an automatic resume.

2. **Live per-stream byte telemetry is absent.** The drain threads in `src/supervisor.rs:1246-1354` get their byte counts only when `std::io::copy` finishes and put them solely into the final receipt. For an active worker, `src/cli.rs:433-562,622-756` reports file mtime/output age but no observed stdout/stderr byte counts; `show` similarly includes byte counts only after an exit receipt. This does not meet the WP03 supervision requirement to record observed byte counts and last-output time during execution. Add a fake worker that emits bytes on both streams and remains alive; assert `status`/`show` expose nonzero per-stream observed counts and recent output while the attempt is still running, then assert final receipt totals and logs after a verified stop. Keep live counts observational rather than presenting them as power-loss-durable byte totals.

## Review checklist and verification

Anti-patterns: dead code PASS (new public execution and control surfaces have production callers); synthetic-fixture FR coverage PASS for claim/worktree/launch/stop/resume, FAIL for the live-byte telemetry in finding 2; silent empty return PASS (no unreasoned silent failure accepted in the reviewed paths); FR coverage FAIL for the active-byte observation in FR-006; frozen surface N/A (no WP03-frozen files identified); locked decision PASS (no observed automatic retry/unassignment or fabricated exit); shared-file ownership PASS with coordination needed for `src/state.rs`, `src/config.rs`, `src/main.rs`, `src/daemon.rs`, and `dev-docs/config-and-state.md`, which are shared with WP02 on the sequential lane; production fragility PASS (no uncovered transient panic/raise identified in reviewed production paths). No `contracts/` artifact exists for this mission.

`CARGO_TARGET_DIR=/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/wp03-root-target`: `cargo fmt --all -- --check` PASS; `cargo test --offline --locked --all-targets -- --test-threads=1` PASS (all test binaries, one intentionally ignored helper); `cargo clippy --offline --locked --all-targets -- -D warnings` PASS. Test log: `tmp/wp03-independent-all-targets.log`; clippy log: `tmp/wp03-independent-clippy.log`. No remaining synthetic `__supervise` or `__worker_gate` workers observed after tests. Existing passing tests do not cover the two cases above.

WP04 depends on WP03; rebase the dependent lane after resolving these findings if it has diverged.
