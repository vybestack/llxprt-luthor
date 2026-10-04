# Issue #10 validation

Verified `vybestack/llxprt-luthor#10` was OPEN and assigned to `acoliver` before implementation and before publishing. GitHub CLI identity and SSH authentication to `github-acoliver` both identify `acoliver`. The mapped base `work/luthor-issue-to-pr-daemon` was at `1b63a8756d77a9a7fb584775ecfadf49a9fb4a8e`, matching the task worktree's starting HEAD; default branch remains `bootstrap/luthor-base`.

## Delivery

All three constructed worker prompts (initial launch, stopped-session resume and audited natural-exit retry) now require separate complete `Tracker-Issue` and `Fixes` lines. Closing references derive from the saved candidate: `Fixes #N` for same-repository mappings, `Fixes OWNER/REPO#N` for cross-repository mappings. The existing provenance parser and author/base/head/claim checks are unchanged. README documents that merging to the maintained non-default base does not itself guarantee automatic issue closure.

Behavioral regressions inspect actual launch/resume/retry plans for both mappings, including misleading worker-template closing text. PR verification regressions confirm both references are accepted, a wrong tracker line is rejected and closing references alone never prove provenance.

## Validation

Rust/cargo 1.98.0; offline, locked checks after deliberate `cargo fetch --locked`.

All paths are absolute and inside this task worktree:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export LUTHOR_RETRY_TEST_TMPDIR="$PWD/tmp/f"
```

Cwd remained the assigned worktree throughout. `tmp/f` is a physical short fixture directory, not a symlink or external `/tmp` workaround.

Passed:

- `cargo test --offline --locked --test supervisor prompt -- --test-threads=1`: 6 passed, including all three new mapping regressions.
- `cargo test --offline --locked --test pr_evidence`: 6 passed.
- `cargo test --offline --locked --test retry_cli -- --test-threads=1`: 2 passed.
- `cargo xtask ci`: exit 0, including policy/debt, formatting, gate fixtures, strict Clippy, build, full serial all-target tests, doctests and all required exact contracts.
- `git diff --check`.

Retained ignored logs: `tmp/focused-supervisor.log`, `tmp/focused-pr-evidence.log`, `tmp/focused-retry-cli.log`, `tmp/xtask-ci-final.log`, `tmp/xtask-ci-final.exit`.

Earlier validation evidence is retained: `tmp/xtask-ci-initial.log` records a CLI fixture failure with the longer fixture-root name; the short physical path focused run passed. `tmp/xtask-ci-short-fixtures.log` records a timing-sensitive failure in the unchanged `natural_exit_stale_assignee_is_held_despite_matching_open_pr` test. Its exact focused rerun passed (`tmp/focused-stale-assignee.log`) and the complete final CI run passed without changing that test, checks, debt ceilings, lint thresholds, required contracts or safety rules.
