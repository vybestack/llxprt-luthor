# Issue 40: opt-in verified-head merging

## Acceptance and safety

- Mapping-local merge authorization is disabled by default. Enabled policy selects
  merge/squash/rebase, all/GitHub/named checks, and explicit draft promotion.
- Strict merge health requires selected checks to pass or skip; missing, pending,
  neutral and failed checks block. Failing names and full summaries are retained.
- Fresh immutable PR ownership and saved exact head must match. Server-side
  `sha` refuses a racing push. A durable intent precedes writes; terminal evidence
  includes the merge commit, merged head, policy and observed check summary.
- Green BEHIND PRs use GitHub `updatePullRequestBranch` via gh with explicit
  `updateMethod: MERGE` and `expectedHeadOid` pinned to the observed head. Each
  update is journaled. Existing claim/receipt/worktree/base-merge lineage checks
  must verify the new head before a later merge sweep can evaluate its checks.
  Conflicts remain with the existing bounded repair path, never rebase/force.
- Exact merged-PR proof precedes any tracker closure. The saved tracker selection
  identifies the issue, including cross-repository and non-default-base cases.
  Already-closed issues need no write; otherwise the comment names PR, head and
  merge commit, and confirmed closure is recorded on the task.
- Uncertain merge/update/close writes are surfaced, not retried or hidden. Restart
  recovery uses fresh exact proof, never a blind replay. Disabled policy makes no
  merge/update/closure reads or writes, including recovery writes.
- Cached status/show exposes checks, merge evidence, updates, closure and errors.
  No operator configuration/state, worker executable, gate, contract, lint
  threshold, branch protection, timeout or assertion was relaxed.

## Continuation review

Attempt `attempt-6e1aa9cbf9ecf85bc01e4e47defcc404` left 39 issue-scoped staged
files on `a580e0e`; these were preserved and reviewed from disk. The live issue
and both comments were re-read. The prior implementation lacked tracker closure
and BEHIND updates; this continuation adds both and their regressions.
Authenticated GitHub identity was verified as `acoliver`. The mapped base was
still `a580e0e` at review. Only the assigned task worktree was changed.

Historical logs were retained: `tmp/issue40/ci-run2/exit` is 0;
`tmp/issue40/ci-final/exit` is 101 with ten pre-existing supervisor
wall-clock/fixture-publication failures. These are not claimed as validation of
this continuation. Fresh validation is below.

## Fresh validation

Rust **1.98.0**, stable worktree cwd, absolute confined directories:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export RUSTUP_TOOLCHAIN=1.98.0
```

The short fixture root and relative state paths follow the Unix-socket guidance;
production bounds were not changed. Logs are under `tmp/issue40/resume/`.

- Structural policy: pass.
- Strict workspace/all-target/all-feature Clippy: pass (`-D warnings`, denied
  cognitive and type complexity).
- Coordinator merge regressions: 24 pass, including default-off, stale/racing
  head, pending/failed checks, draft policy, immutable ownership, interrupted
  writes, closure recovery and BEHIND update/new-head check sequencing.
- GitHub merge/check/update/closure regressions: 15 pass, including real gh argv
  assertions for expected-head MERGE updates, exact-head merges, cross-repo
  tracker closure comments, malformed replies, changed identities and failures.
- Config integration suite: 34 pass.
- Existing supervisor base-merge head-advancement regressions: 8 pass, including
  foreign/non-base rejection, multiple base merges, claim/receipt/clean-tree
  verification and required-failure conflict repair after an update.
- Real execute-cycle merge/closure regression: 1 pass.
- Read-only GitHub schema introspection confirmed `expectedHeadOid` and
  `updateMethod` on `UpdatePullRequestBranchInput`; no live merge, branch update
  or issue-close mutation was performed during validation.

**Full unchanged `cargo +1.98.0 xtask ci`: exit 101**
(`tmp/issue40/resume/ci/log`). Policy, formatting, xtask gate fixtures, strict
Clippy and locked build passed. The serial application tests stopped at the
unchanged `tests/project.rs:323` test
`direct_issue_caches_repository_and_validates_identity_and_urls`: its two
otherwise identical issues had `observed_at_unix_secs` values **1791988014**
and **1791988015**, crossing a wall-clock second boundary.
`tests/project.rs` and `src/github/project/` are unchanged by this PR.

The exact focused rerun passed, exit 0, without changing any assertion, timeout
or production code (`tmp/issue40/resume/project-focused-rerun.log`):

```sh
cargo +1.98.0 test -p luthor --test project --all-features --offline --locked \
  direct_issue_caches_repository_and_validates_identity_and_urls \
  -- --exact --test-threads=1
```

Per the task instruction for unrelated timing failures, the full gate was not
rerun. It stopped before the remaining serial application suites, doctests and
exact contracts; hosted CI must run and decide the complete unchanged gate.


OCR review is skipped per Andrew (2026-10-09). Hosted Ubuntu/macOS quality gates
remain required before any merge; this task creates only an open PR.
