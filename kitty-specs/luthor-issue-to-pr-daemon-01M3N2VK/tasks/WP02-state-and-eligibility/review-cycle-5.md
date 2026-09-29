---
affected_files: []
cycle_number: 5
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T08:27:00Z'
reviewer_agent: user
wp_id: WP02
---

# WP02 review: changes required

For the WP02 implementer: both reported runtime behaviors are corrected in the root checkout. `Config::from_json` rejects `--prompt "PRIVATE-TOKEN: DEMO_VALUE"` without echoing the dummy value; `StateStore::create_task` refuses the same invalid config before writing a task or selection evidence. With the existing local fake `gh` and successful Project, repository, and direct issue replies in `tmp/wp02-review-proof/gh`, `luthor discover --config tmp/wp02-review-proof/date-config.json` exits 1 with `project PROJECT item ITEM has unsupported configured marker field Due`, rather than exiting 0 with empty output. No GitHub writes were made.

**Remaining blocker from feedback-4: the requested combined-path regression test is absent.** `tests/project.rs:107-126` exercises `GhProjectReader::page` for Date `Due`, while `tests/eligibility.rs:442-493` supplies an already constructed `ProjectItem` to `select` for the configured and unrelated-field cases. `tests/cli.rs:1-193` runs the CLI with fake `gh`, but its fixtures contain no Date field or `Due` marker. Thus no committed test exercises a successful fake Project page plus direct issue reads through the production `GhProjectReader` -> `select` -> `discover` diagnostic, nor the unrelated Date field through that same combined path. Add a `tests/cli.rs` fake-`gh` regression test using a configured `Due` Date field and valid direct reads that asserts nonzero exit, an explicit `Due` diagnostic with Project/item identity, and no candidate stdout. Add the unrelated-Date-field case with a configured supported marker and assert selection JSON is printed. This is the end-to-end test requested in feedback-4, not a new behavior requirement.

Root checks passed: `cargo fmt --all --check`, `cargo test --offline --locked` (61 integration tests), `cargo clippy --offline --locked --all-targets -- -D warnings`. No `contracts/` artifact exists. WP02 anti-pattern checklist: dead code N/A for foundation APIs awaiting WP03; synthetic-fixture test FAIL for the Date selector's combined production path; silent empty return PASS for the observed Date path; FR coverage PASS for other original WP02 behavior but Date end-to-end regression remains missing under FR-002; frozen surface N/A; locked decision PASS for credential exclusion; shared-file ownership PASS with WP03 rebase coordination; production fragility N/A. WP03 depends on WP02 and should rebase after the correction lands. No code was edited in this review.
