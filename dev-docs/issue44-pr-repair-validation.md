# Issue 44: verified-PR repair continuations

Re-read issue 44 and its scope-widening comment in `vybestack/llxprt-luthor`.
The comment requires failed-job reruns, flaky-looking versus repeat-failure
classification, and log excerpts as well as the original merge-conflict repair.

## Behavior and safety

- Execute daemon cycles monitor retained `pr_complete` tasks after startup
  reconciliation and cleanup, before new issue dispatch. Preview remains read-only.
- The fresh exhaustive open-PR lookup must match the saved immutable PR ID,
  repository/base/head/author/tracker identities and verified head SHA. Changed
  claims, lookup errors, ambiguous PRs, changed config or unverified process,
  worktree, session/environment or receipt evidence hold the task without repair.
- GitHub GraphQL health is pinned to the same PR ID and head. Paginated required
  check contexts use `isRequired`, not a guess based on check names. Optional
  failures, unknown mergeability and pending checks do not authorize coding.
  Old CheckRuns superseded by a newer ID in the same suite/name are excluded;
  a suite's advanced runAttempt alone cannot turn an old failure into a new one.
  Completed Actions jobs are separately pinned to their own run attempt, ID,
  head and name; old jobs wait rather than authorizing repair during a rerun.
- Each failed Actions run is rerun once per verified head. Durable rerun intent
  precedes the POST; an ambiguous/failed write holds, never automatically retries.
  The writer freshly checks `acoliver`, run repository, head and completion before
  POST. A separate acknowledgement records a successful POST; interrupted or
  unacknowledged intent holds without retrying the write. Passing after a newer run attempt is audited as **flaky-looking**, not
  proof of a particular root cause. A repeat failure authorizes coding with
  failing check names and bounded job-log tails marked as untrusted diagnostics.
  Unsupported non-Actions required failures are held for a person, not silently
  skipped or rerun with an invented API.
- At most three repair continuations per task, across restart. The bound holds
  with the failing check names and leaves all attempts and evidence intact.
- Repair uses the same session, worktree inode, branch, original base provenance
  and environment, with a new attempt ID and distinct prompt. The prompt mandates
  fetch + merge (no rebase/force push), preservation of both sides' behavior/tests,
  patch/checksum regeneration when applicable, unchanged gates and same-PR push.
- An authorized live repair is not stopped just because its PR already exists.
  Claim and PR identity monitoring still apply. Receipt/process accounting occurs
  before result or completion acceptance. After a repaired result, a fresh PR and
  claim verification must match the live local tip. PR verification history stays
  append-only; capacity, cleanup and CLI summaries use the current attempt proof.
- Workers write the attempt-bound, uncommitted
  `.luthor-pr-repair-result.json` with `repaired` or `semantic_conflict` status.
  Missing/stale/malformed reports fail closed. A semantic conflict is durably
  blocked for a person, including after reconciliation/restart; changing the
  report afterward cannot silently unblock it. No automatic re-dispatch of an
  uncertain launch, reassignment, PR merge or issue closure is introduced.

## Validation

Rust 1.98.0; absolute worktree-local `CARGO_HOME=.../tmp/cargo-home`,
`CARGO_TARGET_DIR=.../tmp/target`, `TMPDIR=.../tmp/f`. Stable worktree cwd and
relative fixture state roots keep production Unix socket limits unchanged.
Dependencies fetched once with `cargo +1.98.0 fetch --locked`; all validation
uses locked/offline Cargo. Logs and read-only GraphQL evidence are in ignored
`tmp/`. No private Luthor config/state or worker executable was edited.

Behavioral tests exercise real gated supervisor continuations, same-session
identity, active repair monitoring, post-push head verification, bounds across
restart, semantic holds, stale/missing reports, changed claim/config/author/head,
missing previous receipt, rerun-once errors and passing-rerun classification.
Reader tests cover pagination, pending/optional contexts, malformed snapshots,
superseded check runs, authenticated rerun writes and bounded/erroring log reads.
The PR records final focused and unchanged full-gate results. No contracts,
quality policies, thresholds, timeouts or existing assertions were weakened.
OCR review skipped per Andrew 2026-10-09; hosted CI remains required before merge.

### Historical local validation blocker (resolved on continuation)

Focused repair lifecycle tests: **10 passed**. Required-check health reader tests:
**4 passed**; rerun writer/log tests and existing PR reader tests passed. Focused
CLI tests, Clippy and unchanged structural policy passed.

The exact unchanged `cargo +1.98.0 xtask ci` was invoked twice. Both invocations
were terminated by the shell tool at **120000 ms**, without a reported failing
test. The second requested `timeout_seconds=1200`, but the tool returned:

```text
command timed out after 120000 ms (requested timeout 1200s; effective timeout 120s)
```

Both logs end at the unchanged all-target serial test gate. The intended full
command exit-status file was never written. Detailed evidence is retained in
ignored `tmp/ci-hard-cap-blocker.txt`, `tmp/ci-tool-interruption.txt`, `tmp/ci.log`
and `tmp/ci-full.log`. This is **not a completed or passing full gate**, nor a
reported unrelated timing-test failure. Per the task's blocking-tool-error rule,
work stops here without a detached/background workaround, commit, push or PR.
Resume requires a tool runtime long enough to finish the exact unchanged gate;
hosted CI will still be required before merge. All changes remain in this task
worktree on the authorized branch.

### Continuation review after attempt-49bbd3e35def1a71964d073bdeedb012

Re-read the worktree, issue body/comments, retained diff and validation logs.
Added per-job attempt verification (a CheckSuite runAttempt advances on old
jobs), acknowledgement for rerun POSTs, a no-follow bounded result-file read,
and fail-closed existing-PR repair completion instead of the ordinary absent-PR
retry path. Completion now records its lookup proof transactionally with the
verified PR proof so a restart cannot strand a successful attempt. Regression
tests cover these cases, semantic holds across reopening state, and conflict
repair while checks are pending. The historical tool-interruption evidence above
is preserved; the continuation's final focused and full-gate results follow.

Final focused validation on the continuation: **15 repair lifecycle tests**,
**13 GitHub PR reader unit tests**, and both cached-PR CLI regressions passed.
The unchanged structural policy and strict Clippy passed. Final review also
checks that reconciling an already verified repair does not write a second
completion proof and that configuration/context errors durably hold the task.

The continuation's foreground tool supports sufficient runtime: the exact
unchanged `cargo +1.98.0 xtask ci` completed with **exit 0**, including all-target
serial tests, doc tests and every exact required contract. It was rerun after
the final review edits, with the final output retained as `tmp/final-ci.log`
and exit status in `tmp/final-ci.exit`. The prior attempt's two 120-second
interruptions were tool limits, not accepted passes or test failures. No
background workaround, threshold change, test weakening or gate modification
was used. Hosted Ubuntu/macOS CI is still required before merge.
