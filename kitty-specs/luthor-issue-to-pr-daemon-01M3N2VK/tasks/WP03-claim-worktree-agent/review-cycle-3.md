---
affected_files: []
cycle_number: 3
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T13:38:26Z'
reviewer_agent: user
wp_id: WP03
---

# WP03 independent review: changes requested

Reviewed the authoritative root checkout `work/luthor-issue-to-pr-daemon` at `e7c6ee2de6a1294ef2c1279d7d2beda064eae281`. No code or status event was edited by the reviewer. These are WP03 acceptance issues; WP04's final PR-completion proof is not requested here.

1. **Log-write failure does not stop the worker or record a stop.** In `src/supervisor.rs:1000-1011`, either log drain thread returns on a copy/sync error. The supervisor checks the thread results only after the child exits (`src/supervisor.rs:1013-1041`). An ongoing worker whose stdout write fails can remain active indefinitely; a stdout pipe can also fill after the reader exits. No durable stop intent or stop evidence is recorded. This violates WP03's explicit log-failure stop/hold and fake-child failure requirements. Propagate asynchronous log errors into the supervisor control loop, durably record the failure/stop decision, stop the verified group, and leave the task held without a clean receipt. Inject a log-write or sync failure while the fake worker continues emitting output and assert that it is stopped/accounted for, never marked clean, and the slot is retained until termination is proven.

2. **The one-shot `dispatch` path skips restart reconciliation.** `src/main.rs:225-244` directly calls `dispatch_one` after opening the state store. `src/coordinator.rs:350-416` does not call `startup_reconcile_all`; only `schedule_candidates` calls it (`src/coordinator.rs:437-459`). With capacity greater than one, a pending previous attempt or unfinished source intent can coexist with free capacity, so `dispatch --execute` can claim and launch another task before those intents are reconciled. Route every executable dispatch entry point through pre-dispatch reconciliation and refuse new dispatch on unresolved source/process evidence. Test `dispatch` with an older uncertain attempt or source intent, capacity two, and assert zero assignment writes and worker starts.

3. **Claim verification stops Project pagination at the first matching item.** `src/claim.rs:72-111` breaks when it sees the first target item, even when `has_next_page` is true. A later page containing a duplicate/conflicting target item or a failed Project read is never inspected. `claim` uses `fresh` before and after its single assignment (`src/claim.rs:164-185`), so it can treat incomplete membership evidence as verified and dispatch. Require exhaustive target membership verification on every claim/prelaunch/resume read, including later-page failures and duplicate/conflicting items; add fake Project pagination tests proving neither scenario results in a verified claim or worker launch.

4. **The continuation prompt need not direct inspection of interrupted work.** `src/supervisor.rs:256-294` adds the same mandatory instruction text on initial and resume runs, and `prepare_resume` at `src/supervisor.rs:417-450` checks only a nonempty/different configured prompt. A valid resume template may say only `Continue {attempt.id}`, with no instruction to inspect files left by the canceled turn. The architecture's WP03 session contract requires this instruction because the canceled model turn is not restored. Add mandatory resume-only inspection guidance and assert it appears even with a minimal configured resume template.

Verification from the root with `CARGO_TARGET_DIR=/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/wp03-root-target`: `cargo fmt --all -- --check` PASS; `cargo clippy --offline --locked --all-targets -- -D warnings` PASS; `cargo test --offline --locked --all-targets` failed once at `tests/supervisor.rs:870` (worker marker did not appear within the test's polling window), then the isolated case and full suite PASS on rerun. The intermittent failure needs stabilization or a verified explanation; it is not the basis for the four code findings above. Logs are in `tmp/wp03-review-verification/`. No `contracts/` artifact exists.

WP prompt anti-pattern checks: dead code PASS (new modules have production call sites); synthetic-fixture test PASS for the exercised claim, worktree, supervision and control paths but coverage gaps above remain; silent empty return PASS (no unexplained silent empty result in examined WP paths); FR coverage FAIL (FR-003 incomplete Project reads and FR-006 log failures); frozen surface N/A (none declared); locked decision FAIL (pre-dispatch reconciliation and log-error stop contract); shared-file ownership PASS with this coordination note: `src/main.rs`, `src/coordinator.rs` and `src/supervisor.rs` overlap mission work, so coordinate changes against approved WP02 behavior and ensure dependent WP04 rebases if required; production fragility N/A (Rust `raise` does not apply).
