# Issue 114: update behind-base PRs before checking their stale heads

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/114

## Scope and preserved requirements

The merge coordinator now requests the existing base-branch merge before reading
required checks when a verified PR is behind, mergeable and not a draft. Checks
on that stale head cannot authorize a strict merge, so pending or failed stale
checks no longer delay the update. The branch-update eligibility predicate lives
with the existing followup operations to keep the coordinator within unchanged
complexity limits.

Identity validation and authenticated-account verification still precede the
update. The existing writer revalidates ownership/account and pins the GraphQL
`updateMethod:MERGE` request with `expectedHeadOid`. No rebase, force update,
admin bypass, check rerun or worker launch was added to the merge path.

The existing durable branch-update intent and `pr_branch_updated` evidence are
unchanged. An uncertain update is never blindly repeated. A changed head still
requires the existing PR repair claim/receipt/local-tip/base-lineage verification
(including #104); a branch-update intent is not proof of a new head. Only the
verified head with all required checks passing can authorize the eventual merge.

Drafts are never updated, including when draft promotion is allowed by merge
policy. Conflicting and unknown-mergeability PRs retain their existing waiting /
repair behavior. Non-behind PRs retain the existing check and merge gates.

Work continued from attempt `attempt-a121ed961a5b74784ae7d34541777d76` after
inspecting its uncommitted tests and incomplete compilation log. All inherited
changes were preserved and completed. Only this task worktree was edited; no
private configuration/state, worker executable or other task/source checkout
was changed. GitHub identity was verified as `acoliver`. The mandatory mapped
PR base is `main`.

## Behavioral coverage

Eight new coordinator regressions cover:

- Pending stale-head checks: update immediately, with zero check reads.
- Failed stale-head checks: update immediately, with zero check reads.
- Durable update evidence pinned to the original verified head and no merge
  authorization before verification of the updated head.
- No update for drafts under either draft-policy setting.
- No update after PR identity / verified-head drift or authenticated-account drift.
- No update or merge when mergeability is false or unknown.
- Unchanged pending/failed-check blocking for non-behind PRs.
- Conflicting PRs remain waiting without branch updates or merge authorization.

The existing behind-update lifecycle test additionally proves that a required
failure on the newly verified head blocks merge; only clearing that failure
allows the new head to merge. Existing wire tests prove explicit merge method,
expected-head pinning, fresh identity/account checks and surfaced upstream errors.
Existing real-Git integration tests exercise the repair/base-lineage path and
refuse foreign heads and unverified or dirty work.

## Validation environment and results

Rust **1.98.0**, macOS aarch64, stable repository cwd:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
cargo +1.98.0 test --offline --locked --lib coordinator::pr_merge -- --test-threads=1
cargo +1.98.0 test --offline --locked --lib github::pull_request::merge_write_tests::followup -- --test-threads=1
cargo +1.98.0 test --offline --locked --test supervisor pr_repair::head_advancement -- --test-threads=1
cargo +1.98.0 xtask ci
```

Builds, cached dependencies, fixture directories and logs stay under ignored
worktree-local `tmp/`. Existing confined-relative Unix-socket fixture paths and
production path limits are unchanged. Validation is offline and locked, using
the dependency cache left by the prior attempt.

- Before the fix: **3 regressions failed, 4 passed** (pending stale checks,
  failed stale checks and draft updates reproduced),
  `tmp/issue114/continuation/regression-before.log`.
- Focused merge coordinator: **35 passed**, including eight new regressions and
  the extended updated-head check test,
  `tmp/issue114/continuation/pr-merge-final.{log,exit}`.
- Existing merge-followup wire tests: **5 passed**,
  `tmp/issue114/continuation/merge-write-tests.log`.
- Existing real-Git head-advancement tests: **15 passed**,
  `tmp/issue114/continuation/head-advancement-tests.log`.
- First full unchanged gate: **exit 1** at structural policy, reporting
  `inspect_open` cyclomatic complexity 26 over limit 25. The update eligibility
  predicate was then factored into the followup module; no ceiling was changed.
  Evidence: `tmp/issue114/ci/{log,exit}`.
- Final full unchanged **`cargo +1.98.0 xtask ci`: exit 101**, in its own
  detached session, `tmp/issue114/ci-final/{log,exit}`. Structural policy,
  formatting, gate fixtures, strict Clippy and all-target/all-feature build
  passed. Workspace tests passed 180 library tests (two existing ignored tests)
  and the preceding integration binaries, then the coordinator binary finished
  with **140 passed, 1 failed**. Its sole failure was the unchanged
  `continuation::amendment::lifecycle::safety_launch::ready::safety_already_run_amended_observation_still_accepts_dirty_and_descendant_worktrees`:
  `tests/coordinator/waiting.rs:35`, `fixture receipt not published`, with no
  supervisor-error artifact and an empty supervisor log. The existing receipt
  wait is bounded at **10 seconds**. This test exercises amended-observation
  reconciliation, not the PR merge path; no relevant fixture/supervisor code
  changed. Later workspace binaries, doctests and exact contracts were not
  reached by this full run.
- Exact focused rerun of that unchanged timing-bounded test: **passed**, one
  test, 11.37 seconds, `tmp/issue114/continuation/coordinator-timing-rerun.{log,exit}`:

  ```sh
  cargo +1.98.0 test --offline --locked --all-features --test coordinator continuation::amendment::lifecycle::safety_launch::ready::safety_already_run_amended_observation_still_accepts_dirty_and_descendant_worktrees -- --exact --test-threads=1
  ```

  Per the task instruction, the full gate was not rerun after this unrelated
  timing-only failure and passing focused rerun. Hosted CI decides. No timeout,
  threshold or assertion was changed.

No tests, timeouts, thresholds, debt ceilings, lint settings, contracts, required
checks, branch protection or safety rules were removed or weakened. OCR review
is skipped per Andrew 2026-10-09. Hosted Ubuntu/macOS CI runs the same gates and
is required before any merge. No merge or issue closure is part of this task.
