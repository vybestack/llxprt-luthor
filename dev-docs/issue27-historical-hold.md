# Issue 27: historical unknown attempts stay held

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/27

## Decision and scope

Choose **permanent hold with controlled retirement of the entire queue**, not a
historical capacity-release operation. There is no operator attestation,
`--force`, evidence import, or legacy adoption API. Releasing an unknown attempt
would claim more than the preserved evidence establishes. A future historical
release protocol would need a separately reviewed specification and tests;
this change does not authorize one.

The issue reports two distinct predecessor classes:

| Synthetic predecessor class | Missing proof | Required disposition |
| --- | --- | --- |
| Issue #12 / `attempt-d3c12e5590f529890229b0e10ed5c8bc`: a pre-owner-protocol `launch_intended` attempt, legacy branch-shaped argv, no registration | No contemporaneous attempt-bound ownership/dispatch proof | Keep the reservation and unknown outcome; do not continue, amend, retry, resume, abandon, clean up, or adopt it |
| vybestack/llxprt-code-rs#390 / task `task-5f9d19d92d15963a9c7598ba6cb9fcd4` / `attempt-09d6563d1defca15b786045bd679beed`: owner-proved dispatch, registered child, empty logs, stale stop socket, Broken-pipe supervisor panic, no receipt | Neither a terminal dispatch-abort marker nor verified execution/exit evidence | Keep the reservation and unknown outcome; do not synthesize an abort or an exit |

These are issue-reported facts, not a fresh observation of private state. Tests
use only hermetic synthetic predecessors. This work does not read, edit, recover,
or launch either live attempt, or run a replacement queue.

**Keep #331 separate.** The reported two-slot configuration with one reserved and
one unreserved slot is arithmetic capacity, not dispatch authorization. The unsafe
shared `attempts/` storage, invalid native initial argv and unknown deployed
Luthor-binary provenance are independent blockers; this change neither repairs nor
certifies the live queue. Do not dispatch #331, merge PR #24 or stacked PRs, or use
#23's amended resume/retry as a workaround for either historical hold.

Process absence is only useful when the relevant process inventory and original
ownership protocol are proved. A fresh exclusive lock, a reboot, no observable
PID, empty logs, a stale socket, repaired permissions, a current executable hash,
a GitHub issue closure or a PR merge is **not** that proof. Historical receipts,
owner records, abort markers, launch intents and gate records must never be
manufactured from those observations. Missing/ambiguous evidence retains the
hold; database errors remain errors, not successful dispositions.

## Closing the undispatched loophole for new attempts

Absence of dispatch records alone cannot distinguish a never-started modern
attempt from incomplete historical telemetry. Schema version 5 adds
`launch_preparations`, with **no migration backfill**. The initial planner holds
the actual worktree-owner lock and captures its protocol (task/attempt binding
and root-checked lock inode/device and nonce) before the transaction creating the new
attempt, reservation and original launch intent. That transaction also records
the preparation proof bound to the exact launch sequence. Any failure rolls
back all four database records. A leftover lock file after a failed preparation
is not a preparation proof.

Only the new-initial-attempt creation path can insert this proof. A low-level
unproved launch intent, including an old migrated intent, does not qualify. The
public `prepare_initial` planner captures the lock; coordinators that already
hold it pass that same owner into the internal planner. There is no reconstruction
of preparation evidence from existing reservations, files or schema version.

Undispatched eligibility requires this contemporaneous proof. Production
continuation also compares it with the currently held owner; the initial and
amended dispatch transactions check the same protocol before writing dispatch
history. Abandonment acquires and retains the owner through its immediate
transaction, revalidates the undispatched proof in that snapshot, and only then
may retire a genuinely new, never-dispatched attempt. A replaced lock or another
attempt's proof cannot authorize it. Existing terminal-exit and dispatch-abort
requirements remain unchanged. Missing preparation proof does not invalidate an
already verified terminal exit; it does prevent an **absence-only** exception.

Abandonment of an attempt-free selection is still supported. Successful modern
undispatched abandonment appends `task_retired`, retains original launch and
attempt bytes/outcome, and releases only through the existing retirement
transaction. Replay is refused. This is not a historical release path.

Recovery now checks original persisted ownership before interpreting attempt
storage or probing process absence. Both historical classes still refuse ordinary
missing-receipt recovery. The managed Broken-pipe predecessor is not reclassified
as the new dispatch-abort outcome merely because its child has disappeared.

## Controlled retirement of a queue (external operational contract)

Queue retirement is **decommissioning**, not `luthor abandon` and not a database
phase or capacity transition. There is intentionally no command that converts
its unknown reservations into available slots. An authorized operator must:

1. Record a retirement decision identifying the canonical state root, queue
   identity, effective configuration/build lineage, every reserved task/attempt,
   known process identities, all old/aliased/direct storage routes, outstanding
   external claims and possible delayed PR effects, and the evidence gaps. State explicitly that their
   outcomes and possible worker activity remain unknown. Keep this record outside
   the immutable attempt history; do not insert terminal evidence into it.
2. Disable and fence *all* launch/restart entry points for that queue: daemon,
   service manager, manual execute/resume/retry/continuation, supervisors and any
   older coordinator binary across every old, alias and direct-root route. Maintain
   continuous exclusion after the boundary is established. Stopping a daemon or holding today's lock alone is
   not a fence for an unregistered or detached worker. Have the runtime owner
   enforce isolation of that queue's execution environment and credentials from
   future work. If the complete boundary cannot be enforced, leave the queue held
   and do not authorize a replacement that could overlap it.
3. Once writers are fenced, preserve a consistent, read-only forensic archive of
   the complete root (including SQLite/WAL state), configuration/build identities,
   owner files, logs, sockets, worktrees and Git identities. Record archive hashes,
   the isolation actions, actor, reason and time in the separate retirement
   record. Obtain authoritative accounting of external claims and PR effects; an
   unresolved writer or delayed external effect retains the hold. Do not delete
   locks, chmod evidence to make it appear original, run
   cleanup, rewrite argv, move reservations, reset branches, or erase the old root.
   Normal coordinator opening can migrate state; do not reopen this archive with
   a writable coordinator just to inspect it.
4. Keep the old queue permanently non-executable under the current contract.
   Its reserved-slot count remains reserved in the archive and in the retirement
   accounting. Do not describe it as capacity recovered, quiescent, abandoned,
   completed, dispatch-aborted, telemetry-lost with proven exit, or retryable.
   Restoring the archive does not grant authority to start it.

A separate replacement queue is allowed **only after external isolation is
established**, with a new state root, worktree namespace, execution identity and
explicitly allocated resource budget. It must not rediscover either unknown
tracker task or reuse its repository/worktree/branch/session ownership. Use a
separate, non-overlapping selection scope; fresh state or fresh capacity numbers
are not justification for overlapping execution. Retained unknown reservations
are not deducted from resource accounting as if their workers exited. If these
conditions cannot be satisfied, do not start the replacement. This PR neither
creates such a queue nor changes private configuration, assignments or services.

## Regression evidence and validation

`tests/historical_hold.rs` covers the two synthetic predecessor
classes, exact original bytes and complete database history preservation,
repeated refusals, unregistered live and subsequently absent processes, storage
files/symlinks/modes and explicit repairs, fresh and replaced owner locks,
preparation rollback, cross-attempt proof rejection, modern undispatched
abandonment/replay, SQL evidence errors, and repeated real SIGKILL/reopen cycles.
The ownership/fork fixtures run in their own integration binary under a fixture-lifetime
process-isolation guard, so unrelated pre-exec children cannot inherit their locks.
No fake terminal evidence is introduced by recovery, retirement, release, retry,
or resume. Direct coordinator continuation and amendment regressions also prove
repeated refusals before remote reads or either launch boundary, with every row
and reservation unchanged. GitHub readers are not consulted to override missing
local proof.

`tests/state/preparation.rs` covers v4 migration without backfill or historical
row/sequence changes, retained capacity after restart, foreign-key integrity,
and a conflicting schema failing without accepting or repairing it. Existing
v1/v2/v3 migration assertions still preserve records and capacity and now expect
version 5; v3 fixtures explicitly omit the table that did not exist then.

All fixtures, Cargo home/build outputs and temporary directories stay beneath
the task worktree's ignored `tmp/`. Rust 1.98.0 and the repository's unchanged
`cargo +1.98.0 xtask ci` gate are required. See the PR validation section for the
actual run results, including any unrelated timing failure and focused rerun.
No safety, lint, debt, timeout, assertion, CI or protection threshold is relaxed.
OCR review is skipped per Andrew 2026-10-09; hosted CI is required before merge.


### Completed task-worktree validation

Fresh continuation validation used Rust **1.98.0** with the absolute worktree-local
Cargo, target and fixture directories above:

- `historical_hold`: **13 passed**, including both direct continuation entry
  points and six real SIGKILL/reopen cycles across the two predecessor classes.
- `state`: **27 passed**, including migration without backfill and rollback.
- Supervisor continuation-proof tests: **11 passed**. The original renamed-attempt
  mutation is retained alongside a new preparation-preserving history mutation.
- Structural policy and strict workspace/all-target/all-feature Clippy: **pass**.
- Full unchanged **`cargo +1.98.0 xtask ci`: PASS, exit 0**, in its own detached
  session. Formatting, gate fixtures, strict Clippy, all-target build, serial
  workspace tests, doctests and every exact required contract passed. Evidence:
  `tmp/issue27/resumed-ci/{log,exit}` and `tmp/issue27/resumed-focused/`.

The prior worker's earlier failed fixture runs and successful final full gate
remain in ignored `tmp/issue27/`; none was erased or relabeled. This fresh full
run needed no timing-test exception or gate rerun. Hosted CI remains required
before any merge.
