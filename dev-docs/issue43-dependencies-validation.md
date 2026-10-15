# Issue 43: dependency-aware scheduling validation

## Verified scope

Re-read `vybestack/llxprt-luthor#43` ("Respect dependencies between ready issues
when scheduling") and the current configuration, eligibility, GitHub reader,
claim, scheduler, daemon and operator code before implementation. Base HEAD was
`a57fceecfe1989f303904f36521d942841648125` on the mapped base
`work/luthor-issue-to-pr-daemon`. The assigned task worktree initially had no
changes. GitHub authenticated identity was verified as `acoliver`; origin is
`git@github-acoliver:vybestack/llxprt-luthor.git`.

The issue explicitly permits configured ordering instead of importing GitHub
relationships. This implementation adds source-local `dependencies` rules,
including repository-qualified issue/PR prerequisites and an explicit per-rule
`allow_closed_unmerged_prs` opt-in (false by default). It does not import
blocked-by/sub-issue relationships automatically or modify live operator config.

## Behavior and safety

- Every direct prerequisite must be satisfied. An open issue/PR blocks,
  independent of readiness, Project membership, assignment or local task state.
- Closed issues and merged PRs satisfy prerequisites. Closed-unmerged PRs remain
  blocking unless explicitly allowed. The PR merge result is read directly and
  identity/state inconsistencies are errors, not absence or success.
- Discovery removes blocked candidates from selection and exposes their blocking
  references/states. The scheduler also rechecks before consuming IDs/capacity;
  dispatch rechecks before task creation and during claim/prelaunch verification.
  A prerequisite reopening cannot authorize a worker from stale discovery.
- Waiting creates no task, assignment, worktree or reservation. Independent ready
  issues can still run. A failure after task creation retains the existing hold
  and reservation safety model; no dependency observation clears a hold.
- `discover`, `status`, and daemon preview/execute show waiting issues. Explicit
  daemon targets may wait rather than fail merely because a prerequisite is open.
  External read failures are separately visible; malformed evidence fails closed.
- Status task/PR evidence stays cached. Only dependency waiting uses live,
  read-only GitHub discovery when rules are configured. No dependency reporting
  performs assignment, worker launch, close or merge operations.
- Existing configurations omit/serialize no dependency field when empty. Rules
  are part of existing source snapshots, not a silent live-policy override for
  old task selections. No database migration is required.
- Invalid/duplicate/self/cyclic rules and conflicting or omitted rules on
  overlapping sources are rejected. Cycles can span repositories/projects.

## Behavioral regressions

`tests/dependencies.rs` exercises the real `GhProjectReader` against a confined
fake gh executable, and runs real CLI subprocesses for waiting output:

- two ready phases are not selected together; the next phase unblocks on closure;
- an open prerequisite outside the Project blocks only its dependent;
- every prerequisite must satisfy, including same-number cross-repository refs;
- closed-unmerged PRs block by default; explicit opt-in and actual merge unblock;
  open PRs still block with the opt-in;
- malformed/mismatched identity, state or merge evidence fails closed;
- permission failures remain visible and do not authorize the dependent;
- status/discover and targeted daemon preview/execute show waiting without writes;
- invalid/conflicting/cyclic config is rejected and old config remains compatible.

`tests/coordinator/dependencies.rs` verifies forbidden effects against the actual
coordinator: dependency reopening at each of four dispatch checks never launches;
read failure before task creation writes/launches nothing; blocked work consumes
no slot or task and does not prevent an independent dispatch.

Existing fake ProjectReader implementations explicitly reject unexpected
dependency reads, rather than supplying permissive defaults.

## Validation environment and results

Rust **1.98.0 (88d9e12ae 2026-08-18)**, stable task-worktree cwd, absolute paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export RUSTUP_TOOLCHAIN=1.98.0
```

Dependencies were deliberately fetched with `cargo +1.98.0 fetch --locked`.
Fixture state roots preserve the repository's Unix-socket guidance; production
path limits are unchanged. All builds, fixtures, logs and temporary evidence stay
in this worktree's ignored `tmp/`. No gate, lint/debt/structural ceiling, assertion,
timeout, required check, contract or protection was relaxed.

Focused checks passed:

- `cargo +1.98.0 test --offline --locked --test coordinator --test dependencies --test eligibility --test discovery --test discovery_failures --test config --test cli -- --test-threads=1`:
  coordinator 129, dependency 10, eligibility 16, discovery 11, discovery failures
  3, config 34 and CLI 35 tests passed (`tmp/issue43/focused.log`).
- Final dependency suite after adding the two-ready-phases regression:
  **11 passed** (`tmp/issue43/dependencies-final.log`).
- `cargo +1.98.0 xtask policy`: passed after refactoring module boundaries and
  validation complexity; policy thresholds remained unchanged.
- `cargo +1.98.0 clippy --all-targets --all-features --offline --locked -- -D warnings`:
  passed during focused development; the full gate checks the final tree again.

The full unchanged **`cargo +1.98.0 xtask ci` passed (exit 0)** on its first run,
in a detached session. This includes formatting, xtask policy/contract tests,
strict workspace Clippy, all-target build, serialized all-feature workspace
unit/integration tests, documentation tests, and the required targeted gates.
There were no timing-test exceptions or full-gate reruns. Log/exit evidence is
`tmp/issue43/ci/{log,exit}`. `git diff HEAD --check` also passed; the final status
and HEAD diff were reviewed before commit. No gate/config/contract/threshold files
were modified. OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same
required gates and must pass before any merge.
