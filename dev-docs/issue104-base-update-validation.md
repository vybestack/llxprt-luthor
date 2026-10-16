# Issue 104: base updates after a completed PR repair

Verified tracker: https://github.com/vybestack/llxprt-luthor/issues/104
Repository: `vybestack/llxprt-luthor`; base: `work/luthor-issue-to-pr-daemon`.
Initial task HEAD: `ea03a0e5afa9244c5c2540d41fc42466bad5aa8a`; status clean.
Authenticated GitHub identity verified as `acoliver` before writes.

## Requirements and current-base diagnosis

A conflict repair followed by Luthor's head-pinned base update must reach the
normal exact-head, green-CI merge path, rather than hold forever because the
local task worktree still has the repaired tip. Historical identity/tip holds
must also recover through fresh proof. Foreign commits and non-base merges
must remain held. No receipt, ownership, claim, lineage or merge gate is bypassed.

The current base already limits `verify_repair_tip` to completion writes, and
already proves first-parent base-merge-only lineage and fast-forwards the owned
task worktree before adopting a changed PR head. Reproducing the full sequence
with a real supervisor receipt exposed an earlier blocker: the required
`.luthor-pr-repair-result.json` remains untracked after a successful repair.
The clean-tree check rejects that report before it can prove lineage, producing
exactly `PR repair held: worktree identity or tip did not verify`.
Previous head-advance tests did not start from a completed repair.

The new regression failed on the unchanged production code with that exact
hold; diagnostic status was `?? .luthor-pr-repair-result.json`. Evidence is in
ignored `tmp/issue104/reproduction-diagnostic.log`.

## Implementation

- Preserve the report; do not delete it, blanket-ignore it, or skip worktree
  verification for completed repairs.
- Resolve the latest attempt's unique authorization, successful completion and
  repaired result from durable evidence, refusing blocked or incomplete repairs.
- Allow only the matching retained report alongside an otherwise clean tree.
  Read it through the same bounded, no-follow, inode-checked reader used at
  repair completion. Invalid types, malformed/oversized data, unknown fields,
  stale attempts, semantic-conflict results and other dirty work still fail.
- Recheck before fetch, immediately before fast-forward and afterward. Existing
  PR identity, claim, owner/receipt, first-parent/base ancestry and local lineage
  checks are unchanged. Fast-forward uses `--no-overwrite-ignore` so even an
  incoming base file cannot overwrite an ignored retained report. An update
  intent alone never authorizes a foreign head. The worker report and old completion remain intact.
- Share the report reader without introducing a supervisor/worktree dependency
  cycle. Separate fixture PR identity reads to keep structural ceilings intact.

## Behavioral coverage and validation

New supervisor integration scenarios exercise:

1. A completed conflict repair followed by multiple legitimate base merges.
2. Luthor's real merge coordinator requesting an exact-old-head update, repair
   monitoring adopting the new head, pending CI refusing a merge, then green CI
   merging exactly the updated head once, with no new repair or reservation.
3. Restart and restoration of the historical worktree/tip hold.
4. Foreign commits and non-base merges still holding, even after a recorded
   Luthor update, without moving the local tip or merging.
5. Dirty/unverified reports and unrelated operator work remaining unchanged.
6. A plausible untracked report without a completed repair remaining blocked.
7. Ignored reports still being validated and protected from incoming base files.

Focused checks (Rust 1.98.0, offline/locked, absolute worktree-local Cargo home,
target and TMPDIR, stable repository cwd for relative Unix-socket fixtures):

- `cargo +1.98.0 test --offline --locked --test supervisor head_advancement -- --test-threads=1`:
  13 passed before adding the final two artifact-preservation scenarios.
- `cargo +1.98.0 test --offline --locked --test supervisor head_advancement::after_repair -- --test-threads=1`:
  all 7 new scenarios passed, including eleven dirty/unverified-report cases.
- `cargo +1.98.0 xtask policy`: passed.
- Strict workspace/all-target/all-feature Clippy: passed.

Full unchanged gate: `cargo +1.98.0 xtask ci` **passed (exit 0)**. This
includes formatting, xtask tests, strict Clippy, all-target/all-feature build,
serial workspace/all-target/all-feature tests, doctests, and every exact
behavioral contract. The full supervisor suite passed 243 tests with zero
failures and one pre-existing ignored test; all seven new regressions passed.
No gate rerun, timeout change or timing-test exception was needed.

Full log and exit status: ignored `tmp/issue104/full-ci/{log,exit}`. The first
detached launch did not survive long enough to begin the gate (empty log, no
exit file, PID absent); its evidence was preserved in `tmp/issue104/ci/` and
the successful gate was launched in a fresh log directory and polled to exit.
Other focused logs remain in ignored `tmp/issue104/`.
OCR review skipped per Andrew 2026-10-09. Hosted CI runs the same gates and is
required before any merge. No merge or tracker closure is performed by this task.
