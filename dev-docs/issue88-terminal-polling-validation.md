# Issue 88: terminal PR settlement and bounded GitHub polling

## Scope and acceptance

Finished history must not make idle GitHub cost grow forever. Execute cycles now
settle existing and new `pr_complete` tasks using a lightweight, exact-identity
terminal read, independently of worker-mapping drift and opt-in cleanup. A merged
PR becomes `pr_merged`; a closed, unmerged PR becomes `pr_closed`. Numeric identity,
repository, author, branch, tracker reference and verified head remain mandatory;
a merged advanced head still needs the existing base-only merge-lineage proof.
External delivery does not invent Luthor merge authorization. Terminal phases are
excluded from subsequent merge, repair, cleanup and restoration candidate sets.

When the current tracker mapping enables merging, merged deliveries confirm
tracker closure before retirement, including held repairs. Closed-unmerged PRs do
not close the tracker. Interrupted terminal-close intents retain their exact
proof and target; recovery reads for confirmation rather than replaying an
uncertain write. Tracker writes retain fresh `acoliver`/PR/issue authorization.
Configured cleanup has its normal safety-checked opportunity before retirement;
disabled, quota-deferred or safely refused cleanup does not prevent retirement.
An unfinished cleanup intent still blocks retirement.

Mapping mismatch decisions are local, and the same mismatch is not re-reported.
Merge/repair health polling does not run for such a task. Independent lightweight
terminal probes remain necessary to discover eventual external delivery.
Per-task/per-purpose polling observations persist across daemon restarts, with a
300-second default interval. Pending checks do not trigger duplicate same-cycle
repair health reads. `github_polling.interval_seconds` configures the interval.

Execute cycles bootstrap quota using `gh api rate_limit` and observe subsequent
remaining/reset headers. Optional probes stop at or below the configurable
`github_polling.reserve` (default 500). Claims, dispatch checks, authorized merge
followup and interrupted terminal-tracker confirmation are not vetoed by the
reserve. Cycle reports expose REST and GraphQL adapter request attempts,
including retries and explicit API writes, and the latest remaining/reset quota.
Malformed quota is an error, not an unlimited-quota fallback.

The additive schema-v6 migration introduces `pr_polls` without rewriting historical
selection, attempt, reservation, intent or evidence rows. Historical tasks need
no hand edits. Migration conflicts fail atomically.

## Behavioral regressions

- Upgrade from schema v5 settles a historical same-head external delivery despite
  disabled merge/cleanup and changed worker mapping; subsequent cycles do not
  read the PR again.
- Closed-unmerged and Luthor-merged deliveries settle once; external base-only
  head advancement retains lineage verification and rejects unrelated edits.
- Mapping mismatch produces zero merge reads on repeated sweeps and one local
  diagnostic. Terminal probes are separately bounded.
- Polling deadlines survive repeated daemon invocations; expired observations
  permit fresh checks without changing production timeouts.
- Low bootstrap quota suppresses optional PR polling but permits dispatch;
  response headers can stop later optional probes in the same cycle.
- Held-repair delivery closes the tracker. Receipt reconciliation defers terminal
  PR delivery to the configured cycle settlement pass rather than retiring it
  early and losing the tracker obligation. Its regression failed before the fix.
- Interrupted own-merge tracker closure remains mandatory below reserve even
  after worker-mapping drift, without replaying either merge or tracker writes.
  This regression also failed before the fix. New merges remain mapping-gated.
- Request accounting includes REST writes, GraphQL calls and read retries;
  malformed quota and schema conflicts fail explicitly.
- Continuation review found that merging the two sweep reports lost cleanup
  results from Luthor's own merge. A new regression failed before the fix and
  passes after it: cleanup's missing-receipt refusal is reported, the worktree is
  preserved, delivery becomes terminal, and subsequent sweeps perform no reads.

## Validation environment and evidence

All commands run in the assigned task worktree with Rust **1.98.0**, stable cwd,
and absolute worktree-local paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export RUSTUP_TOOLCHAIN=1.98.0
```

The short fixture root and relative state paths follow the repository's Unix
socket guidance. Builds, fixtures and raw logs stay under ignored `tmp/issue88/`
and other worktree-local `tmp/` directories. No gate, contract, test assertion,
debt ceiling, lint threshold, production timeout or safety rule was relaxed.

Fresh continuation checks:

- Final serial library suite: **161 passed**, two pre-existing ignored tests.
- Focused PR-merge suite after strengthening the mismatch regression: **27 passed**.
- New cleanup-report regression: failed before the fix; **passed** after it.
- Final daemon integration: **15 passed**; state/migration integration: **28 passed**;
  retry-CLI integration: **12 passed**; dependency integration: **11 passed**.
- The combined focused integration call exceeded its 600-second shell-call limit
  during the supervisor suite. Its result is incomplete, not a test pass. Raw
  evidence is `tmp/issue88/continuation/integration.log` and `tool-timeout.txt`.
- The first continuation full gate correctly rejected an 81-line function at the
  unchanged 80-line ceiling. Sweep/cleanup orchestration was extracted into a
  cohesive helper; the unchanged policy then passed. Its log is
  `tmp/issue88/continuation/full-ci/log`.
- The next full gate reached retry-CLI tests and exposed one more missing quota
  fixture response. The fixture now answers the real quota contract; all 12
  retry-CLI tests passed afterward. That failure is preserved in
  `tmp/issue88/continuation/full-ci-2/`.
- The first updated-base full gate then found that the newly integrated
  dependency fixture also needed the quota response. That fixture now preserves
  its strict unexpected-command failure while answering quota; all 11 dependency
  tests passed afterward. The failure is preserved in
  `tmp/issue88/continuation/full-ci-final/`.
- Final unchanged **`cargo +1.98.0 xtask ci`: PASSED (exit 0)**, including
  policy/fmt/clippy, offline locked build, all-target/all-feature serial tests,
  doc tests and the unchanged contract suite. Raw log and exit status:
  `tmp/issue88/continuation/full-ci-final-4/`.
- An earlier run (`full-ci-final-2/`) exposed two real compatibility bugs in
  the implementation: cleanup failure payloads were double-wrapped JSON instead
  of preserving the established `reason` schema, and local repair mapping drift
  was silently dropped rather than reported. Production code now preserves the
  cleanup schema and persists/reports each distinct repair-mapping mismatch once
  without a PR read. History regressions assert preservation of every prior
  evidence row, exactly one new diagnostic, no mutation/hold/launch, and no
  repeated evidence; an intentionally failing PR adapter verifies the local-only
  decision. Focused history: **9 passed**; configured checks: **5 passed**.
- That run also hit the existing fixture-exit receipt timing assertion in
  `retry_restoration_never_bypasses_claim_pr_local_tip_or_session_guards`; an
  unchanged exact rerun **passed**. A later focused cleanup run hit a READY
  handshake timeout in `cleanup_api_failure_and_pr_identity_drift_fail_closed`;
  its unchanged exact rerun **passed**. The other **16 cleanup tests passed** in
  that run. Both failure logs and passing reruns are retained. No fixture wait,
  timeout or assertion was changed. Final full validation is rerun for the real
  production fixes, not to chase the unrelated timing failures.
- `full-ci-final-3/` caught the remaining daemon history test's old assumption
  that a diagnostic leaves no durable evidence. Its replacement retains exact
  equality of all task/attempt/intent/reservation rows and all existing evidence,
  requires exactly one new mapping diagnostic, and reruns repair to assert no
  additional evidence or adapter calls. The complete serial library suite then
  passed again (**161 passed**, two pre-existing ignored).

Before final validation, the task branch was updated to mapped-base commit
`934f828b4e3c2f0d6ba66e203433e9259f57083b` (the base's dependency feature).
Existing task work was preserved in an explicit stash and a full diff under
`tmp/issue88/continuation/`. Index restoration refused changed upstream contexts;
normal three-way content restoration exposed only the expected config-module
import conflict. Both feature imports were retained. All 45 non-overlapping task
files were verified byte-for-byte against the preserved stash before subsequent
issue fixes. The stash remains available; no existing task file was discarded.
The final focused checks and full gate use the updated base.

The prior attempt's fixture failures and logs are retained under `tmp/issue88/`.
The continuation did not edit Luthor operator configuration/state, the worker
binary, the source checkout, or another task worktree. OCR review is skipped per
Andrew 2026-10-09. Hosted CI runs the same gates and is required before any merge.
