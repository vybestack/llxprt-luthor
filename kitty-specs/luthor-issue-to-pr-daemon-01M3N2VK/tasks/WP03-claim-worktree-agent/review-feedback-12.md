# WP03 review feedback

Decision: return to planned. Reviewed target `work/luthor-issue-to-pr-daemon` at `3526c567c9c0983e71d4a0b20f4e186b7eca5806` (the review-claim commit; implementation files were clean).

## Blocking finding: log-drain failure can fail to stop the worker under concurrency

`cargo test --offline --locked --test supervisor -- --test-threads=8` failed on the first concurrent run in `live_worker_log_write_failure_stops_and_holds_both_streams` (`tests/supervisor.rs:461-546`, assertion at line 503): `stdout: Err(StopUnavailable)`. See `tmp/wp03-deepthinker-3526/concurrent-supervisor-1.log`. The same test passes in the full all-target run and in ten isolated reruns, so this is timing-sensitive rather than a consistently failing case. Further concurrent suite repetitions passed initially, then the twelfth run stalled with multiple failures and two `worker-that-must-not-run` child processes still live in `ps` during investigation. The stress job was canceled; afterward the test processes exited. Do not infer a single cause for all failures in that overloaded last run.

In `src/supervisor.rs:1206-1229`, a log-copy error records failure and calls `stop_failed_log_child`; its error exits the supervisor with `StopUnavailable`. At `src/supervisor.rs:1021-1058`, the stop path may return before child/group termination is established when its identity/group probe fails or stop escalation cannot establish absence. The reservation remains held, which prevents unsafe slot reuse, but WP03 requires that a live worker with failed logs be stopped, not just that its slot be held. The observed failed assertion means this guarantee is not established under the requested concurrency test. A live child after the supervisor abandons telemetry has no reliable draining or further stop escalation.

Please isolate the race with repeated concurrent fault-injection tests and identify whether identity observation, group/child reaping, or another resource error caused the `StopUnavailable`. Keep identity-verified signaling only; do not signal a bare PID, release the reservation based on an error, or invent an exit receipt. Ensure that the worker is accounted for or actively stopped after log failure, and add a deterministic test that demonstrates the failure boundary and proves child-group absence (or an explicit, still supervised conservative hold if OS proof is unavailable). Rerun the full offline locked suite, strict Clippy, format check, and repeated parallel supervisor tests. The initial all-target run and strict lint/format passed, so this feedback is confined to the failed concurrency safety case.

## Review checklist and dependency

- Dead code: PASS; WP03 public entry points are used by the CLI, daemon, coordinator, or supervisor, with test hooks called by production wrappers.
- Synthetic-fixture test: PASS; claim, worktree and process integration cases invoke production paths. WP04 owns final PR-completion assertions.
- Silent empty return: PASS; optional CLI live probes display unavailable evidence, and supervisor validation holds rather than silently treating missing evidence as success.
- FR coverage: PASS for WP03 claim, worktree, configured worker, supervision, pause/resume, and local controls; final PR completion belongs to WP04.
- Frozen surface: N/A; no `contracts/` artifact or frozen WP03 file was found.
- Locked decision: PASS; no automatic retry or PR-on-exit completion observed.
- Shared-file ownership: PASS with coordination note: `src/state.rs`, `src/coordinator.rs` and `src/github/pull_request.rs` are shared with WP02/WP04 in the same sequential lane. Keep WP04 on its dependency until WP03 is corrected; rebase dependent work if it has started.
- Production fragility: FAIL at the log-error stop path above. `StopUnavailable` can surface while the worker's telemetry is gone.

No GitHub writes or production-code changes were made in this review.
