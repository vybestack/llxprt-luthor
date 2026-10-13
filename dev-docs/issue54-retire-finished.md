# Issue 54: finished and abandoned tasks held a capacity slot forever

## Cause

A task counts as unresolved capacity in `ensure_dispatch_capacity` until it is
`completed`. Work that finished elsewhere never reached that state:

- A task in `attention` whose issue was closed by other work (a PR merged
  outside Luthor) stayed there, and no command could end it.
- A PR repair pass saw the saved PR as open (the open-PR search lags a merge),
  failed the claim check because the issue had just closed, and held the task
  with `PR repair held: issue claim changed or could not be re-read`.
- A repair attempt that ended after the PR merged found no PR and held the
  task with `post-repair PR or claim verification failed; requires a person`.
  `luthor reconcile` kept answering `repair PR missing, ambiguous or unreadable`.

Each held task kept its slot, so a capacity-1 queue stopped dispatching.

## Behavior now

Terminal phases: `issue_closed`, `pr_merged`, `pr_closed` and `abandoned`
(`state::retirement::RetiredPhase`). A retired task keeps all its rows. Its
phase changes, any reservation is released, and one `task_retired` evidence row
records the phase, reason, actor (operators only), observation time, the
previous phase and hold reason, and what was observed. Retired tasks are not
counted as unresolved capacity, are not reconciled again, and cannot be held,
retried or resumed. The worktree and branch are left for cleanup policy.

**What finishes a task.** Both are read fresh:

- the tracker issue, read directly by the identity saved at selection, is
  `closed` (any reason), recorded with its `closed_at`;
- the saved verified PR, re-read by its immutable id and number (not the
  open-PR search), is merged or closed. The record includes the merge commit
  when GitHub reports one.

A merged or closed PR gives `pr_merged` or `pr_closed`, with the issue
observation attached. Otherwise a closed issue gives `issue_closed`, with the
PR observation attached. If neither is established because a read failed, the
task stays as it was and the cycle reports why under `pr_repair.retire_retained`.
Nothing is concluded from an unreadable state.

**What never retires.** Only tasks in `attention`, `held`, `paused` (or
`pr_complete` for an explicit abandon or a closed issue found by the repair
pass) qualify, and only when every attempt has a verified exit
(`attempt_exit` evidence) and no cleanup is in progress. A running task has an
attempt without one, so it is never retired, whatever GitHub says. Tasks
holding for a genuine safety conflict on an open PR with an open issue are not
finished and stay held.

**Where it happens.**

- Execute cycle, before the blocked check, cleanup and scheduling: after the
  #51 restoration (which still moves tasks held with the old generic reason
  back to `pr_complete` when the exact-identity proof agrees), the sweep
  retires every qualifying `attention`, `held` or `paused` task. This is the
  restoration of tasks already held for `PR repair held: issue claim changed or
  could not be re-read`, `post-repair PR or claim verification failed; requires
  a person`, the generic reason, or sitting in `attention`.
  `pr_complete` tasks are left to cleanup, which also removes their worktree.
- PR repair pass: a failed check on a `pr_complete` task first looks for a
  finished task. A closed issue retires it; a merged or closed saved PR leaves
  it for cleanup (the pass reports it as skipped) instead of holding it.
- Repair reconciliation (`finish_pr_repair`, and a repair already held for a
  person): before holding, the saved PR and the issue are read; a finished task
  is retired and the attempt reported as completed.

The cycle still prints one JSON document. Its `pr_repair` object gains
`retired` (task, phase, reason) and `retire_retained`.

## Operator command

```text
luthor abandon TASK --config PATH --actor LOGIN --reason TEXT --execute
```

For work that must stop while its issue stays open (wrong scope, duplicate).
It records the actor and reason in the `task_retired` evidence, releases the
slot and prints `github_changes: none`: the issue, its assignment and any PR
are not touched. It is refused for a task that may still be running (an attempt
without a verified exit; run `luthor reconcile` first if the worker is gone),
for a task already retired and for an unknown task.

## Display

`luthor status` and `luthor show` report the terminal phase as `phase`, the
reason as `reason`, and the full record as `terminal`. `terminal` is `null` for
tasks that are not retired.

## Tests

`tests/supervisor/pr_repair/retirement.rs` (coordinator level): every
non-running phase and every hold reason retired when the issue is closed; a
merged or closed PR read by id while the search is never consulted; both
observations kept; genuine holds and unreadable state left alone; a task with
an unexited attempt never retired or abandoned; abandon audit and display; a
retired task cannot be held or reconciled back; the repair pass with the search
lagging an open PR while the issue is closed; a merged PR left to cleanup;
repair attempts ending after a merge or close, and a repair held for a person
retired on the next reconcile.
`src/daemon/tests/retirements.rs` runs whole cycles against the hermetic `gh`.
`tests/abandon_cli.rs` drives the real binary. `tests/project.rs` covers
`closed_at`. The fake `ExitPr` reader in `tests/supervisor/pr_readers.rs` now
answers a missing PR with a 404 instead of panicking, as GitHub does; two
existing hold tests read that PR by id and still assert the same hold.
