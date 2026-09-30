---
affected_files: []
cycle_number: 5
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T14:51:49Z'
reviewer_agent: user
wp_id: WP03
---

# WP03 review: changes requested

Reviewed root checkout `ea449c287d1a431888cccf4d17ca8eec72a7541c` (after the review claim). Formatting, `cargo test --all-targets --locked --offline`, and `cargo clippy --all-targets --locked --offline -- -D warnings` passed with `CARGO_TARGET_DIR=tmp/wp03-root-target`. No `contracts/` artifact exists for this mission.

## Blocking findings

1. **Normal agent commits prevent explicit same-worktree resume (FR-004, FR-007).** `src/worktree.rs:280-311` includes the worktree's current commit SHA (`head`) in `WorktreeIdentity`. `inspect_record` at `src/worktree.rs:107-131` compares the whole identity against the SHA recorded before the first attempt; `verify_existing_worktree` at lines 44-60 is called by `prepare_resume` at `src/supervisor.rs:387-394`. Consequently, after the first agent commits to its assigned branch and the operator pauses it, `prepare_resume` returns a worktree conflict before reserving a continuation. Reproduction is already present in `tests/worktree.rs:251-270`: `git commit` on the task branch makes reopening the recorded worktree fail, even though the path, inode, Git directory and branch are unchanged. The resume tests at `tests/supervisor.rs:1455-1498` leave the branch at its initial SHA. Separate immutable ownership properties from the mutable commit tip; retain the original base/head as historical evidence, permit a committed task-branch tip after an accounted attempt, and revalidate path/inode/repository/branch before launching. Add an end-to-end pause, agent commit, restart and same-session continuation test that verifies the second worker starts in the unchanged worktree and sees the committed files.

2. **An accounted natural exit permanently blocks the next issue despite an empty process slot (FR-006, NFR-001, NFR-003).** The real exit path in `tests/supervisor.rs:704-737` proves `reconcile_attempt` records a nonzero exit and releases its reservation (count zero). However, `src/state.rs:534-565` still counts every non-`completed` task as consuming scheduling capacity unless it is a verified `paused` task. `src/supervisor.rs:1460-1512` leaves a naturally exited task `held`, and `src/coordinator.rs:226-239` performs a fresh PR read only for a verified *stopped* receipt. At capacity 1, run an agent that exits 7 without a PR, reconcile its durable receipt, then offer a different eligible issue: `ensure_dispatch_capacity()` returns `Capacity` with zero reservations, so `daemon` reports `capacity_full` and never dispatches the other issue. Exit 0 behaves the same. The architecture requires an accounted exit without an open PR to become `attention`, without retrying that task; do not reserve a slot after proven process-group termination. Implement the post-exit PR read and accounted `attention` path needed for capacity release, while retaining a slot for uncertain termination or missing evidence. WP04 remains responsible for final matching-PR completion proof, not for making WP03's basic scheduling/exit path usable. Add a fake-child, fake-PR test that asserts no auto-retry, visible exit/reason and other-task progress after verified group termination.

## Review-prompt anti-pattern checklist

1. Dead code: PASS. Production `main.rs` and `daemon.rs` call claim, worktree, PR reader, coordinator and supervisor paths; the supervisor is launched through `ProductionLauncher`.
2. Synthetic-fixture test: FAIL for normal-workflow coverage. Resume tests use a fabricated stop-signal receipt and do not run an agent commit before continuation (`tests/supervisor.rs:1455-1498`); the required real commit-and-resume behavior is missing.
3. Silent empty return: PASS for reviewed WP03 failure paths; failures hold rather than become empty success.
4. FR coverage: FAIL for FR-006/FR-007 behavioral acceptance as described above; assertions cover isolated pieces but not the normal completed exit followed by another task or commit followed by resume.
5. Frozen surface: N/A; no WP03-frozen path identified in the mission spec or work-package prompt.
6. Locked decision: FAIL; retained worktree/session on pause must permit continuation of work left by the agent, including commits, and an accounted no-PR exit must demand attention without consuming proven-free process capacity.
7. Shared-file ownership: PASS with coordination note. `src/state.rs`, `src/main.rs`, `src/daemon.rs`, and related tests overlap the WP02/WP03 lane; keep the WP03 changes on the shared lane and warn WP04 (depends on WP03) to rebase after corrections.
8. Production fragility: PASS for reviewed WP03 paths; no uncovered bare `raise` or equivalent transient-race panic identified.

Do not count an open PR or implement WP04's final PR identity proof as part of these fixes. No GitHub writes or source edits were made during review.
