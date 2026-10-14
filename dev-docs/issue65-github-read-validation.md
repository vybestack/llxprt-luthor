# Issue #65: bounded GitHub reads and isolated discovery failures

## Behavior

Idempotent GitHub API reads (Project pages, repository identity, direct issues,
PR lists/details/health and authenticated identity) use at most three attempts
for HTTP 5xx, HTTP 429 and connection resets. Default waits are 100ms and 200ms.
`gh api --include` exposes `Retry-After`; both delay-seconds and HTTP-date are
supported. A requested wait of up to five seconds is honored. A longer wait
returns the failure immediately for this cycle rather than retrying early or
blocking the queue. Spawn failures and nontransient failures are not retried.
Assignment and Actions rerun writes remain single-attempt and fail-fast.

Execute-cycle discovery reports `candidate_read_failures` with `project_id`,
`item`, `issue`, `http_status` and a bounded diagnostic `code`. It never selects
an unreadable issue. `source_read_failures` includes `project_id`, `http_status`
and `code`; a failed source discards all of its partial enumeration. Other
sources remain eligible, including sources sharing a repository. A later read
failure in a targeted multi-issue pass cannot leave an earlier selection from
that failed source dispatchable.

External discovery failures produce cycle JSON and a successful process exit,
not an empty-success illusion. Reconciliation, restoration, retirement, cleanup
and PR repair for known tasks precede discovery and still run. Malformed JSON,
invalid identity, pagination errors, conflicting configuration, local/state
errors and errored writes/claims retain their existing fail-closed paths. Preview
and explicit strict selection APIs retain their existing evidence requirements.

## Behavioral regression coverage

- In-memory readers: one candidate 500, one Project 500, source isolation with a
  shared repository, and malformed evidence refusal.
- Fake `gh`: three-attempt exhaustion; 500-then-200 Project and direct issue
  reads; PR 429 and connection reset retries; bounded backoff and Retry-After;
  long Retry-After deferral; permanent and local spawn failures; no assignment
  retry after an ambiguous 500; partial-page discard.
- Whole execute cycles: another verified issue dispatches while one candidate
  fails; targeted failures never claim or create attempts; Project failures
  preserve reconciliation, retirement of closed issues and PR repair; CLI exits
  zero with explicit failure JSON and no claims.

## Validation

Rust **1.98.0**, with stable worktree cwd and absolute confined paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/fixtures"
```

Relative fixture state roots preserve production Unix-socket limits. Focused
reader, adapter, discovery and daemon tests, structural policy and strict Clippy
pass. The full unchanged `cargo +1.98.0 xtask ci` result is recorded in the PR
body. Logs and exit files are under ignored `tmp/issue65/`. An initial full-gate
policy failure found an 81-line function; shared validation helpers fix it
without changing any ceilings. No contracts, required checks, tests, lint/debt
limits, timeouts, thresholds or safety rules are lowered. OCR review is skipped
per Andrew 2026-10-09; hosted CI remains required before any merge.

The latest complete gate invocation passed structural policy, fmt, xtask gate
fixtures, strict Clippy and workspace build. Its serial test stage failed only
`continuation::amendment::lifecycle::observation::amended_terminal_binding_rejects_missing_duplicate_tampered_and_replayed_history`
at the unchanged 10-second fixture receipt bound (`tests/coordinator/waiting.rs:35`).
The exact focused rerun passed (1 passed, 59.75s for all history cases). Per the
task instructions, the full gate is not rerun again for this unrelated timing
failure; hosted CI decides. Prior runs also exposed a first-build xtask artifact
race (exact rerun passed) and exact fake-gh read routes missing the new
`--include` flag (fixture routes corrected, all five configured-check regressions
passed without weakening any behavior assertions).

## Continuation review after attempt-5409d58750e10e1b49809a02101d0973

The retained worktree, issue, diff, gate logs and exact rerun results were
reviewed from disk. Fresh Rust 1.98.0 checks passed: read unit tests (3),
execute-cycle discovery regressions (6), discovery (11), in-memory failure
isolation (3), GitHub adapters (7), read retries/write refusal (3), and configured
PR check regressions (5). Formatting, unchanged structural policy, strict
workspace/all-target/all-feature Clippy and workspace doctests also passed.

The fresh daemon suite passed 14 of 15 tests, including the new cycle-JSON
regression. Its sole failure was the existing
`daemon_completion::active_finish::daemon_holds_conflicting_active_prs_without_accepting_completion`
fixture's unchanged 10-second worker-publication bound at
`tests/daemon_completion/active_finish.rs:27`. An immediate exact focused rerun
passed (1 passed, 31.90s across its conflict cases). No timeout or assertion was
changed. Evidence is in `tmp/issue65/continuation/focused.log`,
`daemon-timing-rerun.log` and `remaining-focused.log`. Together with the retained
full-gate receipt-timing failure and passing exact rerun above, this is reported
in the PR rather than repeatedly running the full gate; hosted CI remains
required. The full local gate is **not** claimed green, and stages after its
serial-test failure did not execute within that invocation.
