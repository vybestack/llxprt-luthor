---
affected_files: []
cycle_number: 4
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T14:15:27Z'
reviewer_agent: user
wp_id: WP03
---

# WP03 review: changes requested

Reviewed the authoritative root checkout `work/luthor-issue-to-pr-daemon` at `20cec1680799934a47121050e46cd94fd0cda07b`. No implementation or GitHub changes were made. These findings concern WP03 worktree, operator control and intent requirements; final matching-PR completion remains WP04 scope.

1. **Initial and resumed workers can run on an unverified Git branch.** `src/worktree.rs:363-368` has a live identity comparison that checks the worktree's Git directory, branch and HEAD, but the actual launch paths do not call it. `src/supervisor.rs:316-334,399-424,1586-1597` only check the recorded path and device/inode against disk. Switching the existing worktree to another branch leaves those filesystem values unchanged, so `prepare_resume` can reserve a new attempt and `supervise` can run the agent on the wrong head. Before initial launch and explicit resume, compare the current Git worktree identity against the persisted identity, and check again before gate release so a changed branch cannot be started. Add an integration test using a real worktree: switch its branch without replacing the directory, then assert no new attempt is reserved or agent launched. This is the same branch/worktree identity required by FR-004 and FR-007, not a new PR-completion requirement.

2. **An agent that has never printed output never triggers the required silence warning.** `src/cli.rs:80-83,370-388` derives silence only from nonempty log files. With an active reserved attempt and empty stdout/stderr, `output_time` returns `None`, so `status` permanently reports `output_silence_warning: false` regardless of how long the worker runs. `tests/cli.rs:265-274` explicitly expects the false value in the empty-log case. Use the persisted attempt/launch timestamp as the silence baseline until the first observed output; retain the explicit unavailable reason if a trustworthy start time is missing. Test an active, no-output attempt beyond the configured warning threshold and assert a warning without any stop request. This is the WP03 operator-status and silent-agent acceptance scenario.

3. **Creating the worktree root precedes the durable worktree intent.** `src/worktree.rs:325-344` calls `create_dir_all` for a missing root and changes its permissions before `store.begin_worktree` at line 381. A crash or filesystem error in that gap leaves filesystem changes with no worktree intent, contrary to WP03's intent-before-side-effect rule and NFR-002. Persist the intended root/path/branch before creating or changing the root, while retaining the current preflight and unknown-path rejection behavior. Test an injected interruption after root creation and assert that reconciliation can find the durable intent without adopting any unknown worktree.

Verification at the root, with `CARGO_TARGET_DIR=/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/wp03-root-target`: `cargo fmt --all -- --check` PASS; `cargo test --offline --locked --all-targets` PASS; `cargo clippy --offline --locked --all-targets -- -D warnings` PASS. The tests exercised production claim, Project/PR lookup, worktree, supervisor, scheduling, pause/resume and CLI paths, but did not constrain the three cases above. No `contracts/` directory exists.

WP prompt anti-pattern checklist: dead code PASS (new WP03 modules have production call sites); synthetic-fixture test PASS for existing behavior (integration tests exercise production paths, while the cases above lack tests); silent empty return PASS (no unexplained silent-empty return identified in the inspected WP03 paths); FR coverage FAIL (FR-004/FR-007 do not assert actual branch continuity before resume and the silent-agent operator scenario lacks a passing assertion); frozen surface N/A (none specified); locked decision FAIL (filesystem mutation before worktree intent contradicts NFR-002); shared-file ownership PASS with coordination note: `src/supervisor.rs`, `src/worktree.rs`, `src/cli.rs` and affected tests share the mission lane, so preserve WP02's approved state/eligibility behavior and coordinate dependent WP04 against these changes; production fragility N/A (no Rust `raise` construct). WP04 depends on WP03; coordinate its branch after remediation.
