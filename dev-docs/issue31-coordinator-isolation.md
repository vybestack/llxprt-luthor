# Issue 31: deterministic coordinator fixture ownership on Darwin

## Verified scope and base

Issue [31](https://github.com/vybestack/llxprt-luthor/issues/31) concerns
parallel coordinator tests, not a demonstrated production ownership defect.
The issue and its follow-up comment were reread on the mapped base
`e6d3aa55aea2845074e569ad21d9c9f8b700edc9`. That base has **116** coordinator
tests (the reported older checkout had 115). This change adds two tests;
all 116 existing scenarios and the unchanged gate remain required.

## Retained descriptor/process evidence

A default-parallel baseline reproduced many ownership/refusal failures before
the fixture change. The initial foreground command exceeded the tool's 120 s
execution limit, so its partial output is not claimed as a completed run.
A subsequent completed traced run failed **90 passed / 26 failed**, exit 101.

Temporary diagnostic instrumentation in `WorktreeOwner::inherit_into` recorded
child PID, intended owner fd, every inherited worktree-lock fd, its `F_GETFD`
flags, and its `F_GETPATH` pathname from the actual pre-exec child. Fixture
reacquisition failures recorded their exact state-root pathname. Matches include:

| Pre-exec child PID | Intended owner fd | Unrelated retained fd | Unrelated fixture root (under `tmp/f`) |
| --- | --- | --- | --- |
| 46409 | 12 | 87 | `.tmpuATZcJ/state` |
| 53275 | 53 | 47 | `.tmpBU2fRM/state` |
| 53275 | 53 | 100 | `.tmpHOX2s5/state` |
| 57821 | 85 | 36 | `.tmpPW1OIl/state` |
| 57821 | 85 | 116 | `.tmpqu0Orl/state` |
| 61220 | 81 | 33 | `.tmpwdYJdC/state` |
| 61220 | 81 | 91 | `.tmpm44jO8/state` |
| 61220 | 81 | 4 | `.tmpNGlMUg/state` |

Each unrelated fd had flags **1 (`FD_CLOEXEC`)** and named
`<fixture root>/worktree-task-a.lock`; each corresponding fixture failed
`acquire_existing` with `Busy`. The descriptors are not the intended owner fd
being cleared for supervisor/worker inheritance. Diagnostic fd enumeration
lengthens the pre-exec window; this run identifies actual retained descriptors,
not a measurement of baseline failure frequency. Not every failure was traced
to a child, and that limitation is not hidden.

All temporary production instrumentation was removed. The checked-in
synchronized regression independently holds a child between fork and exec,
reports its PID/fd/flags/device/inode over a Unix socket pair, verifies the inode
against the unrelated owner's protocol evidence, drops the parent owner, and
asserts `Busy` **without retries**. After explicitly releasing exec and joining
the child, the unrelated owner must be immediately acquirable. The intended
owner must still exclude another acquisition until its own parent handle drops.
Thus the behavior is proven using process events rather than a lucky sleep.

Local raw evidence is ignored under `tmp/issue31/`: `baseline-parallel.log`,
`traced-parallel.log`, `traced-parallel.status`, and `descriptors.log`. These
contain disposable fixture identities, not operator state.

## Test-only isolation

`Fixture::new` now acquires a process-wide, thread-bound guard **before** opening
state or creating owners. The guard is the last fixture field, so fixture state
and files drop before another harness thread enters. Multiple fixtures on the
same thread share a counted ownership epoch, avoiding deadlocks in existing
multi-fixture scenarios. A condition variable admits another thread only after
the last nested guard drops. Errors remain hard failures; no lock retry, explicit
production unlock, assertion relaxation, or policy change is added.

The second regression checks nested acquisition, exclusion of another thread,
release after the last guard, and subsequent reacquisition. Every existing
coordinator, continuation and preflight scenario uses this fixture. Other test
binaries and production execution are unchanged. Fixture isolation prevents
unrelated test-owned lock descriptors from being present during another
fixture's fork; it does not change intentional supervisor/worker inheritance.

## Validation environment

All commands run from the assigned worktree with Rust **1.98.0**, stable cwd,
and absolute workspace-local paths:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

`tmp/f` is a physical directory, not a symlink/external `/tmp` workaround.
Production stop-socket path validation remains unchanged. Dependency fetching
was deliberate (`cargo +1.98.0 fetch --locked`); checks use offline/locked
resolution. Focused regression, strict Clippy, policy, repeated default-parallel
suite and full unchanged gate logs are retained under `tmp/issue31/`.

## Receipt-event wait exposed by validation

The first guarded default-parallel run completed **117 passed / 1 failed**,
exit 101 (`parallel-repeat.log`). All owner/refusal failures disappeared; the
remaining failure was `amended_natural_exit_without_pr_keeps_attention_and_no_completion_binding`
in the fixture's existing **10 s receipt deadline**, with an empty supervisor
log. This is the scheduling-sensitive wait described in the issue's follow-up
comment, not an accepted successful run.

The two real coordinator launch fixtures now share an event-based receipt wait.
It fails immediately on an explicit supervisor-error artifact or unexpected
filesystem read error, and reports the awaited path, error artifact and supervisor
log. The prior attempt used a 300 s hang ceiling here. On resumption, the
receipt and owner-release ceilings were restored to the original **10 s**
bounds as required by the latest task instructions; no timeout is increased
in the final changes. It does not retry launch, reconciliation or failed proof;
production's five-second READY timeout is untouched. Receipt waits do not
substitute for owner-release or process-absence assertions.

Two subsequent complete default-parallel runs passed **118/118**. A third
completed **117 passed / 1 failed** (`parallel-final.log`): the amended
coordinator real-worker test immediately reconciled after receipt publication
and received `Held` because its **own intentional supervisor owner** had not
released yet. The fixture now uses the same explicit owner-release barrier as
the other real coordinator launch fixtures. The shared barrier fails on owner
unavailability and retains the original 10 s bound; it does not retry reconciliation.
All reconciliation assertions remain unchanged. These intermediate results
are retained, not reported as final-state successes.

## Prior-attempt validation and blocker

- Focused isolation/inheritance regressions: **2/2 passed**.
- Structural policy and strict workspace/all-target/all-feature Clippy: passed.
- Prior-attempt default-parallel coordinator suite: **118/118 twice**, with no
  ignored or filtered tests (1044.03 s and 920.39 s); `parallel-verified.status=0`.
- Unchanged `cargo +1.98.0 xtask ci`: **exit 101**, blocked in the existing
  `xtask/tests/local_type_gate.rs:92` fixture compilation by a **rustc 1.98.0
  internal compiler error**, `no resolution for an import` in the copied
  unchanged xtask entry point. `full-gate.log` retains the complete compiler
  diagnostics and `full-gate.status` the exit status. Application all-target
  tests, doctests and required exact contracts were not reached by this run.

The prior attempt stopped rather than bypassing the compiler error. Its evidence
is retained in `tmp/issue31/BLOCKED.md`. Before resumption, the operator removed
incremental caches under this worktree's `tmp/` and left
`tmp/issue31/OPERATOR-NOTE.md` directing a rerun of the unchanged gate. No gate,
xtask test, contract, threshold or toolchain was changed. OCR review is skipped
per Andrew 2026-10-09.

## Resumption validation

The retained prior code and logs were reviewed before continuing. Receipt and
owner-release bounds were restored to the base's 10 s without changing their
success assertions. Copies of the prior helper and report are preserved in
`tmp/issue31/resume/`.

- Focused inheritance/isolation regressions: **2/2 passed**.
- Real amended coordinator worker/reconciliation regression: **1/1 passed**.
- Structural policy and strict workspace/all-target/all-feature Clippy: passed.
- Unchanged `cargo +1.98.0 xtask ci`: **passed, exit 0**. All-target application
  tests, doctests and every required exact contract completed successfully.
  The old compiler ICE did not recur after the operator's cache cleanup.
- Default-parallel coordinator run with restored original bounds: **117/118**,
  exit 101, 1117.14 s. The sole failure was
  `amended_terminal_binding_rejects_missing_duplicate_tampered_and_replayed_history`:
  no receipt within the original 10 s bound, `error=None; log=Some([])`.
  No transient unrelated-owner `Busy` or short-circuited refusal failure occurred.
- Focused rerun of that test: **failed**, exit 101, 10.23 s, with production
  `ReadyTimeout` before receipt waiting. This is not reported as a passing rerun.
  The queued second parallel run was not executed because the first failed.

These final results are retained in `tmp/issue31/resume/full-gate.{log,status}`,
`parallel.{log,status}` and `receipt-focused.{log,status}`. No full-gate rerun,
timeout increase, assertion relaxation, scheduling fallback or production change
was used to obtain a pass. The prior two complete parallel passes used the
longer prior-attempt fixture hang guards and are not final-state passes.
Consequently the repeated all-green parallel acceptance criterion remains
unmet on this host under the unchanged bounds. The PR reports this limitation;
hosted Ubuntu/macOS quality gates are required before any merge.
