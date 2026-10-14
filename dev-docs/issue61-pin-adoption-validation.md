# Issue #61: audited pin adoption for legacy sessions

## Behavior and scope

`luthor adopt-pin TASK --config PATH --executable PATH --sha256 HEX --actor LOGIN --reason TEXT [--execute]`
provides an explicit operator-attested adoption workflow. Without `--execute`
it uses a read-only SQLite snapshot and emits a single JSON preview. Execution
rechecks eligibility under the coordinator lock and an existing worktree-owner
lock, commits append-only `worker_pin_adopted` evidence and the task phase in one
transaction, and emits one JSON result. Refusals also produce one JSON document
and a nonzero exit.

The task must have only historical unpinned plans, completed/accounted exits,
no active reservations or cleanup intent, and an available worktree-owner lock.
The supplied executable must equal an initial or resume executable in the saved
selection snapshot, resolve to an executable regular file, and hash to the
supplied SHA-256. Already-recorded and already-adopted pins are refused; this is
not repinning or session-format migration. There are no worker launches, GitHub
writes, issue reassignment, copying or rebuilding in adoption. The optional
configuration-driven daemon auto-adoption is intentionally not implemented;
explicit operator action is required.

Evidence records the actor, reason, supplied path, resolved path/hash, time,
historical attempt IDs and phase transition. Historical launch/dispatch plans,
argv and exit artifacts remain unchanged. Effective build resolution overlays
that audit only for the named historical attempts; subsequent resume, retry and
PR repair plans contain the adopted pin normally. Status/show, lineage checks,
PR repair authorization, dispatch and worker gates use the same immutable build.
Only a hold specifically caused by the legacy unrecorded build is restored to
its prior continuation phase. Unrelated safety holds and incompatible sessions
remain blocked.

## Behavioral regressions

Ten tests cover read-only preview, durable evidence and restart, resume planning,
real retry dispatch/worker-gate/exit reconciliation, and real PR repair after an
unrecorded-pin hold and restart. Refusals cover wrong/malformed hashes, an alias
not in the saved snapshot, an already-recorded pin, repeated adoption, an
unfinished attempt, a real live worker and a live owner despite completed DB
accounting. Other cases cover the saved resume executable, preservation of an
unrelated hold, one-document CLI output, changed bytes between preview and
execution, changed bytes after planning and missing adoption evidence.

## Validation environment and continuation

The continuation first inspected the retained worktree, git status/diff/history,
all existing adoption changes and regression tests, and `tmp/issue61/` logs;
then re-read the open assigned issue using `gh`. Authenticated GitHub identity
was verified as `acoliver`, and the task branch's remote is SSH only.

Rust **1.98.0**, stable worktree cwd, with absolute confined directories:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export RUSTUP_TOOLCHAIN=1.98.0
```

Relative fixture state roots preserve the production Unix-socket bounds. Builds,
logs and temporary files are confined to ignored `tmp/`. No tests, contracts,
timeouts, assertions, lint thresholds, debt ceilings or quality gates changed.

The first fresh focused run had two existing shared-fixture READY timeouts
before adoption (`tmp/issue61/continued-focused.log`); eight tests passed. The
first full unchanged `cargo +1.98.0 xtask ci` then passed all ten adoption tests
and 172 supervisor tests, but exposed an issue-scoped regression: a broad
pre-dispatch selection check returned `State(LaunchBlocked)` earlier than the
existing retry-audit safety test's contracted `ExecutionUnavailable` path. The
code was corrected to check only build lineage at this stage, preserving the
existing downstream audit checks and all assertions. Evidence:
`tmp/issue61/ci/log` and `ci/exit` (101).

After that code correction:

- `cargo +1.98.0 test --all-features --offline --locked --test supervisor two_retries_require_exact_audit_even_when_revision_returns_to_selection -- --test-threads=1`: **pass**, all four scenarios (`retry-identity-fixed.log`).
- `cargo +1.98.0 test --all-features --offline --locked --test supervisor pin_adoption -- --test-threads=1`: **pass**, all ten regressions (`continued-focused-fixed.log`).

**Final unchanged `cargo +1.98.0 xtask ci`: PASS (exit 0).** Policy, formatting,
scanner/gate fixtures, strict workspace/all-target/all-feature Clippy, build,
serial all-target tests, doctests and every exact required contract passed. The
supervisor target passed 173 tests with one existing helper ignored, including
all ten adoption regressions and the unchanged retry-audit safety regression.
Full-gate evidence is retained in `tmp/issue61/ci-final/log` and
`tmp/issue61/ci-final/exit`. The gate ran in its own detached session using the
required Python `os.setsid()` launcher and was polled without reducing any bound.

OCR review is skipped per Andrew 2026-10-09. Hosted Ubuntu/macOS CI runs the same
unchanged gates and is required before any merge.
