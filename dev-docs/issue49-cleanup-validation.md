# Issue 49 cleanup isolation: implementation and validation

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/49

**Latest disposition:** PR #55 repaired after hosted platform failures in the
replaced-worktree diagnostic regression. Ownership verification now checks the
saved filesystem identity before Git discovery. The existing exact cleanup
assertion is unchanged, and a new verification regression covers invalid Git
metadata. The unchanged full Rust 1.98.0 gate **passes** (exit 0), including
all-target tests, docs and every required contract. Earlier failures below are
historical evidence. Hosted CI remains required before merge.

Continued after attempt `attempt-4c029d7e4bcc93837330dc4d5b7f780c` in task
worktree `/Volumes/XS1000/lwself/task-1d5a2ed060a62e0caf60efaa02f3d06a`,
on branch `luthor/task-1d5a2ed060a62e0caf60efaa02f3d06a` at HEAD `d4d20c2`.
Inspected status, existing diff, tests, retained dependency-fetch log, issue
body/comments, and matching PRs. GitHub authenticated identity was `acoliver`;
no matching open PR existed. No GitHub writes, commits or pushes were made.

## Preserved and completed implementation

- Task-local cleanup failures append specific non-secret `cleanup_failed`
  evidence and `cleanup.skipped` entries instead of aborting the pass.
- Status/show expose `cleanup_failure`, superseded by a subsequent success
  without erasing history. Explicit dispatch also includes the cleanup report.
- The daemon does not feed a cleanup-skipped task into PR repair in the same
  cycle, preventing historical configuration failures from blocking dispatch
  again through that path.
- Whole-worktree removal permits ignored, entirely untracked nested fixture
  repositories only under confined `tmp/`. Artifact cleanup still refuses
  nested repositories. Tracked gitlinks, including uninitialized submodules,
  are refused. Existing symlink, mount, special-file, ownership, quiescence,
  unsaved-file, tip and protected-directory rules remain intact.
- Existing refusal tests now assert a task-local skip with the exact diagnostic
  and retained files/no removal evidence instead of the obsolete global-error
  API behavior. No refusal or safety assertion was removed to make a pass.
- Added real-supervisor/CLI behavioral coverage for mixed cleanup and daemon
  scheduling, historical root/selection mismatch, ignored fixture repositories,
  tracked submodules, symlink refusal, persisted diagnostics and history.

## Validation

All commands ran with stable worktree cwd, Rust 1.98.0, and absolute:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

Short physical fixture roots and relative state paths preserve the repository's
Unix-socket fixture guidance; production socket bounds are unchanged.

- Focused cleanup suite: **17 passed**, exit 0
  (`tmp/issue49/focused-cleanup-final.log`).
- Diagnostic sanitization regression: **1 passed**, exit 0
  (`tmp/issue49/diagnostics.log`).
- Structural policy: passed (`tmp/issue49/policy.log`).
- Strict workspace/all-target/all-feature Clippy: passed
  (`tmp/issue49/clippy.log`, run before the final exhaustive diagnostic mappings).
- Formatting passed at the beginning of both full-gate invocations.
- Final `git diff --check`: passed.

The unchanged full command `cargo +1.98.0 xtask ci` **has not passed**:

1. First invocation exited 101 during unchanged `xtask/tests/attribute_identity.rs`
   test `compiled_procedural_names_aliases_and_reexports_are_not_builtin_derives`.
   At `xtask/tests/attribute_support/mod.rs:82`, rustc reported:

   ```text
   error: extern location for xtask does not exist: .../tmp/target/debug/libxtask.rlib
   ```

   Evidence: `tmp/issue49/ci.log`, `tmp/issue49/ci.exit` (101).
   Its unchanged exact focused rerun passed: `tmp/issue49/attribute-rerun.log`.

2. Second unchanged full-gate invocation exited 101 during unchanged
   `xtask/tests/derive_binding_gaps.rs` test
   `compiled_optional_core_alias_is_not_the_compiler_namespace`, again at
   `xtask/tests/attribute_support/mod.rs:82`, while compiling a fixture's
   `gate.txt`. Rust **1.98.0 (88d9e12ae 2026-08-18)** on
   **aarch64-apple-darwin** emitted:

   ```text
   note: no errors encountered even though delayed bugs were created
   error: internal compiler error: no resolution for an import
   error: internal compiler error: `Res::Err` but no error emitted
   error: internal compiler error[E0618]: expected function, found `{type error}`
   ```

   Evidence: `tmp/issue49/ci-final.log`, `tmp/issue49/ci-final.exit` (101).
   The gate stopped before the application all-target tests, doctests and
   required contracts; those stages cannot be reported as passed.

This compiler/tool failure is **not** the allowed unrelated wall-clock/timing
assertion exception. Per the task's fail-fast/tool-blocker instruction, work
stops without substituting toolchains, serializing or changing the full gate,
changing tests/thresholds, or repeatedly rerunning it. No xtask, contract,
workflow, lint ceiling, Cargo manifest/lockfile or safety policy was edited.
All issue work remains uncommitted in the task worktree for continuation after
the tooling blocker is resolved. No PR URL or ID exists for this attempt.

OCR review is skipped per Andrew 2026-10-09. Hosted CI remains required before
any merge. Once validation is unblocked, review the full diff/status again,
commit with command-scoped AC attribution, push only the task branch over SSH,
and create/update the PR against `work/luthor-issue-to-pr-daemon` with both
required tracker/fixes lines and accurate validation results.

## Continuation after attempt-8188cbcd82171f05d2bb3366e189088f

Re-inspected the preserved worktree/status, diff, tests and logs, re-read issue
49, and verified `gh api user --jq .login` returned `acoliver`. No matching PR
was present. Work remains confined to the authorized task branch/worktree.

The prior compiler blocker passed its exact unchanged focused rerun with Rust
1.98.0 (`tmp/issue49/continuation-compiler.log`, 1 passed). Review found one
remaining behavior gap: a historical selection configuration with the **same**
worktree root could clean successfully or be retained, then abort dispatch in
PR repair's full configuration check. Cleanup now checks persisted selection
settings (assignment, sources, mappings, initial/resume templates) as well as
roots. Operational cleanup policy/free-space settings can still change without
invalidating ownership. The mixed daemon regression now includes a separate
same-root/different-selection task in addition to the old-root task, an eligible
fixture-repository task, and unrelated new work. Both historical tasks remain
PR-complete with files intact and specific skipped reasons, the eligible task
is removed, and the new issue is assigned/dispatched.

The first full-gate invocation in this continuation failed structural ceilings
introduced by that extension (cyclomatic 28 > 25 and function lines 101 > 80).
These were fixed by extracting coherent configuration validation and test
assertion helpers, without changing any limits or assertions. Evidence is in
`tmp/issue49/continuation-ci.log` and `continuation-policy.log`.

Fresh validation after that refactor:

- Focused cleanup suite: **17 passed**, exit 0, including the extended mixed
  historical configuration regression (`continuation-focused-final.log`).
- Diagnostic sanitization: **1 passed**, exit 0 (`continuation-diagnostics.log`).
- Unchanged `cargo +1.98.0 xtask ci`: **incomplete due to shell tool timeout**.
  The tool returned `command timed out after 1200000 ms; output:`. The retained
  `tmp/issue49/continuation-ci-final.log` confirms policy, formatting, all xtask
  gate fixtures, strict workspace/all-target/all-feature Clippy, and workspace
  all-target/all-feature build passed. Its last line is:

  ```text
  gate: cargo test --workspace --all-targets --all-features --offline --locked -- --test-threads=1
  ```

  No all-target test result was emitted, no completion/exit file was written,
  and no remaining task gate process was observed after the timeout. The
  driver's unchanged subprocess handling buffers test output, so this evidence
  does **not** identify a particular failing/timing-bounded test. It is not the
  permitted unrelated wall-clock test exception. All-target tests, doctests and
  required contracts cannot be reported as passed.

Absolute `CARGO_HOME`, `CARGO_TARGET_DIR` and short physical `TMPDIR` were set
under the worktree's ignored `tmp/` as documented above. No timeouts, test
thresholds, assertions, gate commands, contracts, manifests, workflows or safety
rules were changed. `git diff --check` passed; the final status and full tracked
HEAD diff were reviewed and saved as `tmp/issue49/continuation-review.diff`.

Per the fail-fast tool-error instruction, stopped at this concrete tool timeout
without increasing the shell timeout, rerunning the whole gate repeatedly, or
claiming complete validation. All issue work is preserved **uncommitted and
unpushed** for continuation. No GitHub writes occurred; no PR URL or ID exists.


## Final continuation after attempt-3b5869cc79d35bcfbab919e471603fa6

Re-inspected the worktree, status, existing diff, regressions and retained logs;
re-read issue 49; verified GitHub identity `acoliver`; found no matching PR.
The issue implementation is unchanged from the preserved final refactor.

With Rust **1.98.0 (88d9e12ae 2026-08-18)** and the absolute confined Cargo,
target and TMPDIR environment documented above:

- Focused cleanup regressions: **17 passed**, exit 0
  (`tmp/issue49/final-focused-cleanup.log`).
- Diagnostic sanitization: **1 passed**, exit 0
  (`tmp/issue49/final-focused-diagnostics.log`).
- Full unchanged **`cargo +1.98.0 xtask ci`**, one shell call with the explicitly
  authorized **5400-second** tool timeout, completed with **exit 101**
  (`tmp/issue49/final-ci.log`, `final-ci.exit`). Policy, formatting, xtask gate
  fixtures, strict Clippy and workspace build passed. All-target tests had one
  failure: **`operator_recovery_does_not_count_nonmatching_pr`**, panicking at
  unchanged `tests/supervisor_support/stop_views/completion.rs:48` with
  **`fixture exit receipt missing`**. Supervisor target summary: **126 passed,
  1 failed, 1 ignored**, including all issue 49 tests passing.
- This test uses unchanged `dispatched_fixture(7)` in
  `tests/supervisor/execution.rs:106-134`, with a bounded wait of **200 iterations
  x 20 ms** before asserting the receipt exists. It runs before the new cleanup
  tests, does not invoke cleanup, and its production/operator-recovery paths
  are untouched by this issue. Its exact focused rerun with the same Rust
  toolchain/all-features/offline/locked options **passed**, exit 0, in **4.57s**:

  ```sh
  cargo +1.98.0 test --workspace --all-features --offline --locked \
    --test supervisor operator_recovery_does_not_count_nonmatching_pr \
    -- --exact --test-threads=1
  ```

  Evidence: `tmp/issue49/final-timing-rerun.log`. No timeout, wait count,
  threshold or assertion was changed. Under the user-authorized unrelated
  timing-test exception, the whole gate was **not** rerun repeatedly.
- The full gate stops on a nonzero all-target stage. Its remaining unchanged
  stages were therefore executed separately, not represented as a full-gate
  pass: `cargo +1.98.0 test --workspace --doc --all-features --offline --locked`
  and `cargo +1.98.0 xtask contracts` both **passed**, exit 0. Each required
  contract ran exactly and passed. Evidence: `final-doctests.log`,
  `final-contracts.log`, `final-supplemental.exit` (0).

Final status and `git diff HEAD` reviewed; `git diff --check` passed. No xtask,
manifest/lockfile, workflow, debt ceiling, lint threshold, contract, protected
branch setting or production safety rule was weakened. OCR review skipped per
Andrew 2026-10-09. The PR must disclose the failed full-gate run and passing
focused rerun, and hosted CI must decide before any merge.


## PR #55 repair: attempt-8f6298ff803bc5192740de0033b46eb8

Inspected a clean task worktree at `38964c5`, retained tests/logs, issue 49 and
PR #55. Verified authenticated identity `acoliver` before writes. PR was OPEN
and CONFLICTING with both hosted matrix checks in progress, no failed checks.
Fetched mapped base `work/luthor-issue-to-pr-daemon` over the specified SSH
remote and merged **`d634469`**, without rebase or force push.

Conflicts in `src/coordinator.rs`, `src/coordinator/pr_repair.rs` and
`src/daemon.rs` were resolved by combining both sides:

- Preserve base's `RepairOutcome`, skip/hold diagnostics, exact finished-PR
  behavior, drift checks, conservative wrongly-held task restoration, and all
  base regressions.
- Preserve cleanup-aware exclusion before any repair inspection. Return those
  exclusions as skipped notes with their original non-secret cleanup reason.
- Run restoration before cleanup; run cleanup-aware repair after cleanup;
  report restoration, cleanup and repair outcomes together.
- Extend the mixed daemon regression to require matching cleanup/repair skip
  diagnostics and no holds. Add a focused regression proving a cleanup-skipped
  task avoids an otherwise failing health read and repairs normally next cycle.

No vendored code, patch, checksum or provenance pin exists in the changed
integration; regeneration was not applicable. No thresholds, assertions,
timeouts, contracts, workflows, manifests or safety refusals were weakened.

Validation with the documented absolute confined Cargo/TMPDIR environment:

- Focused cleanup: **18 passed** (includes the new integration regression),
  `tmp/issue49/repair-cleanup.log`.
- Focused PR repair: 22 passed and 2 unchanged bounded receipt waits initially
  failed (`repair-pr-tests.log`); both exact focused reruns passed without
  edits (`repair-pending-rerun.log`, `repair-config-rerun.log`).
- Base's three daemon repair/restoration regressions: **3 passed**,
  `repair-daemon-tests.log`.
- Structural policy passed (`repair-policy.log`).
- **Full unchanged `cargo +1.98.0 xtask ci`: PASS, exit 0**, in one shell call
  with timeout 5400. Policy, formatting, xtask gate fixtures, strict Clippy,
  workspace build, all-target serial tests, docs and all 31 exact required
  contracts passed. Supervisor target: **136 passed, 0 failed, 1 ignored**.
  Evidence: `tmp/issue49/repair-ci.log`, `repair-ci.exit` (0).
- `git diff HEAD`, diff against fetched base and final status reviewed;
  `git diff --check` passed. All conflict entries resolved.

Update the existing PR only; OCR review remains skipped per Andrew 2026-10-09.
Hosted CI must run on the repaired merge commit and pass before any merge.


## Required-check repair: attempt-570cc5bf19fb6f752998411bdeacd54a

Inspected preserved worktree at `babf370`, status, tests and logs; re-read issue
49 and PR #55; verified GitHub identity `acoliver` before writes. Fetched mapped
base over SSH and attempted the required merge: **already up to date**, so no
new merge commit was needed. No conflicts or vendored patch/pin changes apply.

Fetched failed hosted run **38398155917** via `gh run view --log-failed`.
Both Ubuntu and macOS failed only
`supervisor::cleanup_refusals::cleanup_replaced_worktree_and_reserved_task_refuse`:
expected `worktree repository, path or branch changed`, received `read failed`.
Evidence: `tmp/issue49/hosted-failure.log`. The supplied aggregate-job excerpt
only indicated matrix failure; these matrix logs identified the actual bug.

A renamed/replaced worker directory could reach Git discovery before its saved
device/inode was compared. Depending on whether the enclosing checkout's branch
was discoverable, it produced a specific conflict or generic Git failure. Fixed
`verify_record` and `verify_snapshot` to reject a non-directory, symlink or
changed device/inode before invoking Git. IO/Git errors still propagate: no
catch-and-relabel fallback, error swallowing or weakened verification. The
existing cleanup assertion and no-deletion checks remain unchanged. Added
`replaced_worktree_reports_identity_conflict_before_git_discovery` for both
verification entry points with deliberately invalid replacement `.git` data;
asserts the exact reason and preservation of foreign/moved files.

Validation in the documented confined absolute Cargo/TMPDIR environment:

- Initially the offline check reported missing `proc-macro2` after the cache had
  been cleared between attempts. The documented deliberate `cargo +1.98.0
  fetch --locked` stage succeeded (`check-repair-dependencies.log`), populating
  only ignored worktree Cargo storage; no lockfile/dependency changes.
- Worktree tests: **20 passed** (`check-repair-worktree.log`).
- Focused cleanup/integration tests: **18 passed**
  (`check-repair-cleanup.log`), including the unchanged hosted-failing assertion.
- **Full unchanged `cargo +1.98.0 xtask ci`: PASS, exit 0**, one shell call with
  timeout 5400 (`check-repair-ci.log`, `check-repair-ci.exit`). Includes policy,
  formatting, gate fixtures, strict Clippy, build, all-target serial tests,
  docs and all 31 exact required contracts.
- Reviewed `git status` and `git diff HEAD`; `git diff --check` passed.

No tests, thresholds, timeout, required check, contract, branch protection or
safety rule was weakened. OCR remains skipped per Andrew 2026-10-09. Update
existing PR #55 only and require hosted checks on the repaired commit before
any merge.
