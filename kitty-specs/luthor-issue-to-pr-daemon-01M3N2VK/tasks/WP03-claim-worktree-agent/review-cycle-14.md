---
affected_files: []
cycle_number: 14
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T18:40:40Z'
reviewer_agent: deep-bug-investigator
wp_id: WP03
---

# WP03 independent review: changes requested

Reviewed root checkout: `69213a7d956458a7e85da0d5b97e3fd549112d03` at `/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1`. Scope is WP03 and the two findings in `kitty-specs/luthor-issue-to-pr-daemon-01M3N2VK/tasks/WP03-claim-worktree-agent/review-cycle-13.md`. WP04 completion proof and live GitHub operations are excluded.

## Blocking verification finding

**The stop-race regression does not exercise the required production stop path or the second required ordering.**

Evidence: `tests/supervisor.rs:781-802` makes `dispatched_fixture(7)` wait for a completed natural-exit receipt. `tests/supervisor.rs:1030-1041` then calls `store.record_stop_intent` directly, after reading that receipt. It never calls `request_stop`. Its comment at line 1036 describes exit after intent, but the actual ordering is receipt before intent. This test verifies the revised classification predicate and reconciliation, not the production pause race.

The bypassed production behavior is `src/supervisor.rs:710-761`: durable intent, supervisor identity checks, dead-supervisor fallback, socket connection and response. The signal decision also checks child exit and identity at `src/supervisor.rs:803-828`. The current test could continue passing if either of those production stop paths regressed. Existing cooperative-stop and escalation tests at `tests/supervisor.rs:1776-1820` send signals to a live child; neither covers natural exit before the first signal.

This is a verification gap, not a claim that a new runtime failure has been demonstrated. Removing the zero-stop-intent condition from `src/state.rs:1105-1136` addresses the documented state-classification conflict: a verified receipt with no stop signals can now take the natural-exit attention path despite a stop intent. `src/coordinator.rs:239-247` chooses that path only after receipt reconciliation. That change still needs the two reproductions explicitly required by the finding artifact and this review request.

### Precise reproduction of the current test gap

```sh
cd /Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1
CARGO_TARGET_DIR=/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/wp03-root-target \
  cargo test --offline --locked --test supervisor \
  pause_after_natural_exit_preserves_natural_attention_path \
  -- --exact --test-threads=1
```

Observed: PASS, one test, with the ordering above. Evidence: `tmp/wp03-independent-69213a7/pause-repro.log`. This passing result cannot establish either real `request_stop` race. No code or test source was modified during review.

### Required remediation within the existing finding scope

Add deterministic real-child integration regressions using the existing `running_worker`/`stopped_receipt` fixtures or an equivalent controlled fixture:

1. Receipt-before-stop: release a natural exit, wait for its synced receipt, then call production `request_stop` (or the actual pause CLI), and reconcile with exhaustive absent-PR evidence. Handle the legitimate stop-unavailable result if the supervisor has already exited. Assert the natural receipt is unchanged, `stop_signals` remains empty, the attempt is accounted, capacity is released only after verified group absence, and task state is `attention`.
2. Intent-before-exit-before-signal: hold the worker alive until the production stop intent is committed, then force its natural exit before stop handling can signal it. Assert that temporal ordering rather than describing it in a comment. Complete production stop handling and reconcile the actual receipt. Assert no fabricated stop signals, the same accounted attention state and safe capacity release.
3. For these scenarios, verify status/show agree on the accounted task and attempt, preserve claim/worktree/session, and that an explicit resume is rejected without creating another attempt. No automatic continuation is permitted.

These are the original required reproductions, not additional product scope. Do not replace them with direct intent insertion after an already completed receipt.

## Verified changes and original-scope regression checks

Active telemetry is implemented through the shared `observed_log_bytes` helper at `src/cli.rs:885-905`, which observes safe per-stream file sizes. Both `status` (`src/cli.rs:461-493`) and `show` (`src/cli.rs:686-762`) expose separate stdout/stderr counts, observation time, and the observational qualifier. Missing/unsafe logs produce explicit unavailable counts rather than zero.

`tests/cli.rs:784-870` uses production launch preparation and same-binary gated dispatch for a child that writes to both streams and stays alive. `tests/cli.rs:1061-1125` checks nonzero counts and recent output in both CLI views while their phase is running, then requests a real stop and compares final receipt totals with both log lengths. `tests/cli.rs:203-245` covers missing and unsafe active logs. `tests/supervisor.rs:658-708` separately checks exact captured stream contents, a real receipt, gate evidence, and one-time reservation release. These tests passed. The telemetry finding is resolved at the reviewed scope.

Original claim, PR absence, worktree ownership, gated launch, conservative reconciliation, log-failure stop, capacity, explicit same-session continuation and no-retry paths were read against the WP/spec and exercised by the serial suite. No additional blocker was found in those reviewed paths. No `contracts/` artifact exists for this mission. The root and lane `src/` implementations match; root `tests/config.rs` contains additional documentation-example checks, so verification and verdict apply to the requested owned root checkout.

## Verification evidence

All checks used `CARGO_TARGET_DIR=/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/wp03-root-target`:

- `cargo fmt --all -- --check`: PASS, exit 0. `tmp/wp03-independent-69213a7/fmt.log`.
- `cargo test --offline --locked --all-targets -- --test-threads=1`: PASS, exit 0. Top-level binaries report 187 passed, 0 failed, 1 intentionally ignored helper. Subprocess helper results are additional nested output. `tmp/wp03-independent-69213a7/tests.log`.
- `cargo clippy --offline --locked --all-targets -- -D warnings`: PASS, exit 0. `tmp/wp03-independent-69213a7/clippy.log`.
- Focused pause regression above: PASS, one test. This does not close the identified coverage gap.
- No `__supervise` or `__worker_gate` processes were present before verification or remained after either test run. `tmp/wp03-independent-69213a7/workers-after.log` is empty.

An additional disposable black-box proof command was rejected by the shell tool's safe parser before execution. It supplies no runtime evidence and created no workers. The verdict relies on the inspected source and the completed checks above, not that unexecuted command.

## Prompt checklist

Dead code: PASS for the reviewed execution/control production call chains. Synthetic-fixture check: PASS for the real-child telemetry and classification assertions; the separate stop-path verification gap is described above. Silent empty return: PASS, observation failures have unavailable/held reasons. FR coverage: PASS for the WP's named behaviors at assertion level, with the required stop-race verification incomplete. Frozen surface: N/A, none specified for WP03. Locked decisions: PASS, no automatic retry, unassignment, fabricated natural-exit signal, or exit-based completion introduced. Shared-file ownership: PASS with coordination note: `src/state.rs`, `src/main.rs`, `src/daemon.rs`, configuration tests and `dev-docs/config-and-state.md` are shared with the sequential WP02/WP03 lane; keep their root-only documentation checks when aligning the lane. Production fragility: PASS for the changed predicate and observational telemetry paths.

WP04 depends on WP03. If its lane has diverged, rebase it after these WP03 verification changes. Verdict: request changes for the one blocking verification finding.
