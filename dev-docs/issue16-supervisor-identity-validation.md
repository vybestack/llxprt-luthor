# Issue 16: detached-supervisor exit/reaping race

## Verified scope

Repository `vybestack/llxprt-luthor`, open issue #16, assigned to `acoliver`;
SSH and GitHub API authentication both reported `acoliver`. The task branch is
`luthor/task-ba2a170fc1175f80cd6cb98a0bfacacc`, based on
`work/luthor-issue-to-pr-daemon` at `1b63a8756d77a9a7fb584775ecfadf49a9fb4a8e`.
The issue comments confirm this separately selected supervised task. No issue-2
state, worker executable, operator configuration, or other checkout was changed.

## Reproduction and diagnosis

On macOS, unchanged focused run 32 reproduced the original assertion at
`tests/supervisor.rs:723`: expected `Completed { exit_code: Some(0), signal: None }`,
received `Held { reason: "supervisor identity unavailable" }`.
`tmp/repeated-100-baseline.log` contains that failure. An unchanged complete
`cargo xtask ci` also passed (`tmp/ci-baseline-complete.exit` is 0), consistent
with the issue's intermittent report.

Temporary diagnostic instrumentation reproduced the same failure at run 114
in `tmp/identity-probes-250.log`. After identity lookup failed, the first group
probe found a group and the Darwin zombie probe failed. Immediately rechecking
reported `group_absent_recheck=true zombie_recheck=false` for the same PID.
The instrumentation is not in the patch.

The worker receipt is atomically published before the supervisor returns.
The coordinator's reaper independently waits for the detached supervisor.
Identity lookup (`sysctl` plus `proc_pidinfo`), group probing, and the external
Darwin `ps` zombie query are separate observations. Exit/reaping between the
first group query and the zombie query made the latter fail for an already
absent process. Reconciliation incorrectly treated the earlier group presence
as current evidence and held the task despite a provably absent group.

A preliminary concurrent baseline repetition also saw a startup `ReadyTimeout`
(run 3, `tmp/repeated-baseline.log`). It did not reproduce in the subsequent
isolated repetitions; startup timeouts and gate authorization are unchanged.
No fixture isolation or timeout workaround is used by this fix.

## Fix and safety

After unavailable identity, a present group, and no verified Darwin zombie,
re-probe the exact recorded supervisor group. Only the existing `ESRCH` absence
proof permits progress. A present or inconclusively probed group remains held.
This does not retry/ignore identity mismatch or use receipt existence as process
absence proof. The existing Darwin zombie exception and independent child and
descendant checks are preserved. No provenance assertion or required check is
weakened.

Three deterministic regressions cover probe ordering across reaping, persistent
unavailability, and existing absent-group/zombie behavior. Two end-to-end
regressions first prove an actual released worker receipt, then demonstrate
that mismatched supervisor identity or unavailable identity with a surviving
process group retains the reservation, including after reopening the state.
They use isolated fixture state only. The new tests live under the existing
stop-view test support module without expanding the debt-limited supervisor
test file or changing any ceiling.

## Validation environment

All Cargo commands run from the assigned worktree with absolute
`CARGO_HOME=$PWD/tmp/cargo-home`, `CARGO_TARGET_DIR=$PWD/tmp/target`, and
`TMPDIR=$PWD/tmp/fixtures`, on `/Volumes/XS1000`. Unix socket fixtures keep the
repository-root cwd stable; no external `/tmp` workaround is used. Rust is
1.98.0. Dependencies were deliberately fetched with `cargo fetch --locked`;
validation is offline and locked. An initial foreground gate exceeded the
shell tool's 120-second timeout during compilation; complete gates were then
run in worktree-local background shells with exit receipts.

Passing focused validation:

- 120 consecutive original detached-supervisor tests, unchanged completion and
  worker receipt assertions (`tmp/repeated-fixed.exit`: 0).
- `cargo test --offline --locked --lib supervisor_exit_race_tests`: 3 passed.
- `cargo test --offline --locked --test supervisor identity_races -- --test-threads=1`:
  2 passed.

Full unchanged `cargo xtask ci` passed on macOS (`tmp/ci-fixed.exit`: 0),
including the serial workspace suite and executed contracts. Hosted macOS/Ubuntu
results are recorded in the PR validation report. Build artifacts and detailed logs remain in ignored
worktree-local `tmp/`.
