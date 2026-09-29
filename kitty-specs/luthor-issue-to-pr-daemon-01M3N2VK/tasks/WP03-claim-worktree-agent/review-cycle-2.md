---
affected_files: []
cycle_number: 2
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T12:43:36Z'
reviewer_agent: user
wp_id: WP03
---

# WP03 review feedback

Verdict: changes requested. Scope: WP03 claim, worktree, supervisor, capacity, pause/resume and local controls. Target inspected: `kitty/mission-luthor-issue-to-pr-daemon-01M3N2VK-lane-a` at `e5278ab`. WP04 depends on WP03; coordinate/rebase its work after these changes land. Shared `src/state.rs`, `src/main.rs`, `src/lib.rs` and the lane integration with WP02 need coordinated changes; do not overwrite WP02's approved behavior.

1. **Check checkout and branch feasibility before the assignment write.** `src/coordinator.rs:170-179` calls `claim::claim` before `worktree::ensure_worktree`; the checkout origin, push remote, base ref and branch availability are only checked in `src/worktree.rs:76-113,250-280`. A missing base, wrong checkout or occupied `luthor/<task>` branch thus leaves an assigned issue with no runnable worktree. Move the non-mutating mapping/branch/path preflight ahead of the persisted claim intent and assignment, preserving the post-claim checks against races. Test invalid checkout/base and branch collision with a fake assignment writer and assert zero writes.

2. **Do not mark a stopped attempt `paused` without fresh absent-PR evidence.** `src/state.rs:1006-1023` changes `held` to `paused` solely because a stop intent and nonempty `receipt.stop_signals` exist. `src/main.rs:145-157` and `src/coordinator.rs:55-77` reconcile the receipt without a PR lookup. The architecture's pause contract requires an unambiguous fresh absent PR after stop; PR-present or failed/ambiguous lookup must hold, not make the task resumable. Add this post-stop lookup to the coordinator/control reconciliation path before the paused transition, and test absent, present, and failed PR reads. WP04's full PR-completion proof remains out of scope here.

3. **Make source-intent reconciliation available for tasks that have not yet launched.** `src/coordinator.rs:55-77` only reconciles attempts and labels unfinished claims/worktrees as `source_holds`. `src/main.rs:132-139` requires an attempt before `reconcile TASK`, so a crash after claim intent or partial worktree has no working operator reconcile control. Implement a read-only reconciliation path for claim/worktree intents that inspects current issue/Project and recorded filesystem identity, reports uncertainty without a second assignment or automatic adoption, and keeps dispatch blocked until evidence is resolved. Test crashes before the first attempt and verify the operator can inspect/reconcile those tasks without launching or releasing unverified state.

4. **Implement the required stop and operator evidence contract.** `src/supervisor.rs:808-839` uses TERM then KILL; the specified bounded INT, TERM, KILL escalation is absent. `src/cli.rs:47-77` reports only phase, issue, reserved flag and held reason in `status`, omitting latest attempt/outcome, output age or silence warning, and PR status; `show` provides some of these only through separate queries and has no PR read. Add the required stop sequence and tests (including a child that ignores INT and TERM), and show latest attempt/outcome, live-output age/silence warning and PR lookup state or explicit unavailable reason in the operator controls. Do not turn silence into a kill or infer PR absence from an unreadable adapter.

5. **Restore the static formatting gate.** `cargo fmt --all -- --check` fails on `tests/supervisor.rs:1197,1207`. Format the test and verify the gate without changing assertions.

Verification on the lane checkout: `cargo test --locked --all-targets` passed (the helper test intentionally ignored in the parent harness ran in its child invocation); `cargo clippy --locked --all-targets -- -D warnings` passed; `cargo fmt --all -- --check` failed with the cited diffs. No `contracts/` artifact exists for this mission. No GitHub writes or code changes were made during review.

Prompt anti-pattern checklist: dead code PASS (new modules have production call sites); synthetic-fixture test PASS (claim/worktree/supervisor assertions call production paths); silent empty return PASS (no unexplained silent default found in examined WP paths); FR coverage FAIL (FR-007 stop/post-stop evidence and FR-009 status behavior above lack asserted coverage); frozen surface N/A (none declared); locked decision FAIL (claim-before-checkout and paused-without-PR contradict plan/architecture); shared-file ownership PASS with coordination instruction in this feedback; production fragility N/A (Rust `raise` does not apply).
