# Issue 59: mapping-local PR check gates

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/59

## Verified requirement and scope

Re-read issue 59 in `vybestack/llxprt-luthor` and the mapped base
`work/luthor-issue-to-pr-daemon` at `44a0539`. The health reader selected only
GitHub's `isRequired`; the repair coordinator consumed that selected health.
There is no automatic merge implementation on this base (issue 40 remains
separate). Verified-open-PR handoff still accepts draft/red/pending PRs under
its unchanged contract; this change does not equate `pr_complete` with green CI.

Mappings now support exact `required_checks` names or `{"kind":"all"}`.
Omission retains GitHub selection. The selected health feeds the existing
rerun/repair path; pinning, immutable job-attempt checks, one rerun per head,
repair limits, bounded excerpts and person-required non-Actions failures are
unchanged. Missing named gates are pending. All-check selection excludes
skipped runs and includes legacy status contexts. Status/show expose the cached
source, with `none` for an unprotected/unconfigured repository, `unknown` for
legacy evidence and null before an observation. Old mapping snapshot bytes
remain unchanged when using the default; mapping drift is not silently adopted.

No private operator config/state, installed worker executable, branch protection,
quality policy, contracts, thresholds or timeouts were modified.

## Behavioral regressions

- Real gh adapter with hermetic GraphQL/Actions responses and no `isRequired`
  flags: configured list and all-check policies each POST one rerun, wait, survive
  a store restart, then authorize same-session repair with the selected failed
  check and its log excerpt.
- Default unprotected repository remains idle and reports `none`, without
  Actions reads or writes; default protected checks remain selected.
- Configured rerun success records flaky-looking evidence without a worker;
  configured non-Actions failure holds for a person without reservation.
- Skipped runs are excluded in all-check mode, missing/pending gates wait,
  latest attempts work across pages, and changed job heads fail closed.
- Configuration parsing/round trips reject malformed rules, duplicate/empty
  names and non-string entries; historical mappings preserve their default.
- Status/show distinguish unobserved, historical and no-gate evidence and
  refuse malformed latest health instead of hiding it.

## Validation

Rust **1.98.0**, macOS, in the task worktree only:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

Kept cwd stable and used relative state roots for Unix socket fixtures. The
explicit dependency fetch used the locked manifest; checks ran offline/locked.
Logs and build/fixture artifacts are in ignored `tmp/`.

Passing focused checks:

- `cargo +1.98.0 test --offline --locked --lib github::pull_request -- --test-threads=1`
  (19 passed)
- `cargo +1.98.0 test --offline --locked --test config -- --test-threads=1`
  (29 passed)
- `cargo +1.98.0 test --offline --locked --test supervisor configured_checks -- --test-threads=1`
  (5 passed)
- `cargo +1.98.0 test --offline --locked --test cli cli::checks -- --test-threads=1`
  (2 passed)
- Structural policy and strict workspace/all-target/all-feature Clippy.

The broader focused repair suite initially passed 41/42 tests; the existing
`supervisor::pr_repair::missing_or_stale_repair_result_and_unpushed_tip_fail_closed`
failed at `tests/supervisor_support/stop_views/completion.rs:48` with
`fixture exit receipt missing` after the bounded fixture wait. Its unchanged
exact focused rerun passed (24.63 seconds). The final full gate also passed
that test. Evidence: `tmp/repair-tests.log`, `tmp/repair-timing-rerun.log`.

**Full unchanged `cargo +1.98.0 xtask ci`: PASS (exit 0)**, one shell invocation
with timeout 5400 seconds. It executed policy, fmt, gate fixtures, strict Clippy,
locked build, all-target serial tests, doc tests, and every exact contract.
Evidence: `tmp/ci.log`, `tmp/ci.exit`. Existing opt-in smoke tests remain opt-in;
no claim of live repository/worker acceptance is made.

OCR review skipped per Andrew 2026-10-09. Hosted Ubuntu/macOS CI runs the same
gates and is required before any merge; this task does not merge or close issues.
