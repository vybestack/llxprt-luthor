# Luthor

Luthor is a local Rust daemon for selecting eligible GitHub Project issues, assigning them to a configured login, launching `llxprt-code-rs` in isolated worktrees, and checking whether each task produced a matching pull request. Discovery and operator inspection are read-only with respect to GitHub. Assignment, worker launch, pause, and reconciliation are separate actions with different effects; read the command behavior below before using them.

WP05 delivery is complete, and Luthor completed a separately authorized five-issue live acceptance run. The implementation delivery PR is [Luthor PR #1](https://github.com/vybestack/llxprt-luthor/pull/1); the five issue PRs and their observed check states are recorded in [WP05 validation](dev-docs/wp05-validation.md). Synthetic macOS/Linux dry-run results remain separate from that live evidence. Several issue PR checks are failing or still running, so this acceptance record does not claim green CI.

## Build and test offline

Install a Rust toolchain that supports the Rust 2024 edition. The dependencies are locked in `Cargo.lock`.

```sh
cargo build --offline --locked
cargo test --offline --locked --all-targets -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets -- -D warnings
```

The documented checks require dependencies to already be present in Cargo's local cache. `--offline` prevents Cargo from fetching missing packages. The test command runs the full target suite serially, as used in the recorded macOS and Linux validation. Linux format and Clippy results were not recorded because those tools were unavailable in the Linux image.

The binary is `target/debug/luthor` after a debug build. Use `cargo run --offline --locked -- --help` for the top-level help, or run the built binary directly:

```sh
target/debug/luthor --help
target/debug/luthor daemon --help
```

The actual top-level usage is:

```text
Usage: luthor discover --config <path>
       luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]
       luthor dispatch --config <path> --repository owner/repo --issue N --config-revision REV [--execute]
       luthor resume TASK --config <path> --execute
       luthor retry TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT [--revalidate-terminal-exit] --execute
       luthor recover TASK --attempt ID --config PATH --actor LOGIN --reason TEXT --execute
       luthor abandon TASK --config PATH --actor LOGIN --reason TEXT --execute
       luthor status --config <path>
       luthor show TASK --config <path>
       luthor logs TASK [--attempt ATTEMPT] --config <path>
```

`pause` and `reconcile` are also implemented operator commands; their exact syntax is shown under [Inspect and control tasks](#inspect-and-control-tasks). `recover` is a narrow audited recovery operation for missing receipts, not a routine retry control. `abandon` stops a task for good and is described in [finished and abandoned tasks](dev-docs/issue54-retire-finished.md).

## Configuration and access setup

Luthor requires a JSON configuration. The structs in `src/config.rs` reject unknown fields. The required top-level fields are:

| Field | Meaning |
| --- | --- |
| `state_root` | Private local directory for SQLite state and attempt artifacts. |
| `worktree_root` | Root directory for daemon-managed task worktrees. |
| `capacity` | Positive maximum number of reserved task attempts. |
| `assignment_login` | GitHub login Luthor assigns to selected issues. |
| `sources` | Non-empty set of Project sources and eligibility selectors. |
| `mappings` | Non-empty tracker-to-code repository and push/PR identity mappings. |
| `initial` | Executable and argv template for a new worker session. |
| `resume` | Executable and argv template for continuing that session. |

A source includes `project_id`, `repositories`, a `ready_marker`, and optional `milestone`. Markers are either `{"kind":"label","name":"…"}` or `{"kind":"project_field","name":"…","value":"…"}`. A mapping includes `tracker_repository`, `code_repository`, `checkout`, `base_branch`, `push_remote`, `allowed_pr_head_repository`, and `allowed_pr_author`. Command templates use an executable and an `args` array. They are argv templates, not shell commands, and Luthor does not validate worker flags; the worker validates its own CLI. Luthor rejects only unknown or malformed template variables and credential-bearing arguments. Supported substitutions are `{task.issue_number}`, `{task.repository}`, `{task.issue_url}`, `{task.id}`, `{attempt.id}`, and `{worktree}`.

Do not copy a configuration example into a live environment without validating every repository, Project, account, path, branch, and remote. The configuration contract and a synthetic example are maintained in [Configuration and state](dev-docs/config-and-state.md); validation rules and command-template constraints are enforced by `src/config.rs` and tested in `tests/config.rs`. Keep credentials out of JSON and argv. Luthor uses the installed `gh` executable for GitHub operations and requires its authenticated identity to match the configured allowed identity for the selected task. Do not assume an alternate credential or authentication fallback.

Before any live scheduling, verify all of the following independently:

- The Project ID, allowed tracker repositories, exact readiness marker, and optional exact milestone select only intended issues. Discovery requires Project membership, an open issue, the configured marker, no assignees, and a matching milestone when configured.
- `assignment_login` is the intended assignee and the authenticated GitHub identity is permitted for that task. GitHub assignment is not an atomic compare-and-set operation; Luthor verifies the assignment afterward and holds work when it cannot verify the result.
- Each tracker repository has exactly one valid mapping. Confirm the checkout and base branch, push remote, allowed PR head repository, and authorized PR author against the intended GitHub repositories and account.
- The configured SSH push remote works for the intended identity and repository. Configure SSH outside Luthor; never put credentials in the configuration or command arguments.
- `state_root` and `worktree_root` are local paths with appropriate access controls. Use a dedicated state directory and do not share it between independent Luthor installations.

## Preview and explicit execution

Start with read-only discovery:

```sh
target/debug/luthor discover --config /private/luthor/config.json
```

This calls GitHub Project and issue reads and prints eligible candidates as JSON lines. It does not assign or launch a worker.

Daemon mode without `--execute` performs preview cycles. Supply both `--repository` and `--issues` to restrict the selection to specific configured issue numbers; omit both to scan configured sources. `--once` exits after one cycle. Without it, preview repeats every 30 seconds.

```sh
target/debug/luthor daemon --config /private/luthor/config.json --config-revision local --once
# Optional target restriction, still preview-only:
target/debug/luthor daemon --config /private/luthor/config.json --config-revision local --repository owner/tracker --issues 17,18 --once
```

Only `--execute` authorizes daemon scheduling, including assignment and worker launch. Dispatch also requires `--execute`. These are GitHub and local-state side effects, not dry-run switches:

```sh
target/debug/luthor daemon --config /private/luthor/config.json --config-revision local --repository owner/tracker --issues 17 --once --execute
target/debug/luthor dispatch --config /private/luthor/config.json --repository owner/tracker --issue 17 --config-revision local --execute
```

The examples show syntax only. They are not a recommendation to run against live repositories. Live use requires a separate operational decision and a verified, safe preflight. Luthor claims by assigning the configured login, verifies the claim, prepares a task worktree, and launches the worker. An uncertain assignment or launch is held rather than automatically retried.

## Private state, capacity, and recovery

`state_root` contains `state.sqlite3` and private per-attempt artifacts, including worker logs and process evidence. The state implementation uses restrictive permissions for private artifacts. Keep the root on a trusted local filesystem and back it up only with appropriate protections. A process-level coordinator lock prevents simultaneous coordinators from dispatching against the same state root; database transactions and durable reservations also enforce capacity across restarts. Each task also has a private worktree-owner lock in the state root. Its exclusive file descriptor (FD) is passed from the coordinator through the supervisor to the worker, including across `exec`, so the reservation remains owned while that task's worker is alive even if the coordinator exits. The lock is per task, so separate tasks do not block one another. Luthor records the task ID, attempt ID, and lock file device/inode as evidence and checks that the open descriptor still names the expected file before relying on it.

If the owner lock cannot be acquired or verified, launch is refused and recorded as a bounded launch failure such as `launch_failed`; inspect the task and attempt evidence with `status` and `show`. Older attempts without owner-lock proof do not gain proof retroactively. Recovery of those attempts remains held when ownership or process quiescence cannot be established. Do not replace, remove, or recreate a lock file to clear a refusal: that changes its inode and can let two processes hold locks on different files under the same path. Do not release a task reservation just because the lock path appears free. Require the recorded attempt and process evidence to establish that the worker is accounted for.

This lock coordinates cooperative Luthor workers that follow the protocol. It does not contain arbitrary uncooperative programs running as the same user, which can ignore the lock or interfere with same-user files and processes. Use separate operating-system isolation for that threat boundary. Do not remove lock, reservation, or attempt artifacts to free capacity manually.

Capacity counts reserved attempts, including uncertain attempts. A slot is not released merely because a process appears absent or a worker exited. Luthor retains reservations when process, receipt, claim, or PR evidence is uncertain. Pause can free a slot only after process-group termination is verified; the task's claim, worktree, logs, and session identity remain. A paused task requires explicit resume. Each resume keeps the task's worker session and worktree and creates a new attempt.

A worker process exit does not complete a task. Completion requires a verified matching open PR in the mapped code repository, with the exact tracker issue linkage, expected base and allowed head repository, and authorized identity. Draft status and pending or failing checks are reported, but do not by themselves negate a verified matching open PR. An absent, ambiguous, or unreadable PR result does not establish completion. Luthor does not automatically retry work, review or repair a PR, merge without an explicit mapping policy, or unassign an issue.

## Terminal-task cleanup and disk-space dispatch holds

Cleanup is **disabled by default**. Set the optional top-level `cleanup` policy
in the current configuration to opt in:

```json
"cleanup": {"kind": "artifacts", "paths": ["target", "tmp/build"]},
"min_free_space_bytes": 53687091200
```

Alternatively use `{"kind":"worktree"}` to remove the whole worktree, or
`{"kind":"disabled"}` to retain everything. `min_free_space_bytes` is an
unsigned byte count (the example holds below 50 GiB); it defaults to zero.

Execute daemon cycles and explicit dispatch run cleanup after startup
reconciliation and **before** measuring disk space or selecting new work.
Preview, discovery, status, show, and reconcile do not delete files. Cleanup
only considers `pr_complete` tasks with completed, released attempts. It holds
the existing worktree-owner lock, rechecks receipt/process/group absence and
worktree identity, and reads the exact previously verified PR from GitHub.
Only a freshly verified **merged or closed** PR authorizes deletion. An open,
reopened, or replacement matching open PR retains the worktree. Running,
reserved, held, paused, and attention tasks are never cleaned. Tasks with
missing ownership or quiescence proof fail closed; telemetry-loss and
historical boot-identity uncertainty do not acquire new proof through cleanup.

Artifact paths must be non-empty, non-overlapping relative paths outside
`.git`, ignored by Git and entirely untracked. Cleanup refuses symlinks,
nested repositories, special files, and filesystem boundaries. The one nested
repository exception is whole-worktree cleanup of ignored, entirely untracked
fixture repositories under the worker's confined `tmp/` directory; these are
disposable build output. Tracked submodules are always refused, including
uninitialized ones. Artifact-only cleanup still refuses all nested repositories. Whole-worktree
cleanup also requires no unsaved tracked/untracked files and a local tip equal
to the freshly observed PR head; ignored build output may be removed. It uses
Git worktree removal to remove registration, but retains the task branch,
fetch pin, private task state, logs, session and ownership-lock evidence.
Checkouts and state directories cannot overlap deletion targets.

The task records a durable `cleanup` intent (policy, targets, worktree and
terminal PR proof), one `cleanup_removed` event per removed path, then
`cleanup_complete`. `show TASK` exposes removed paths and completion/failure
events. Both `status` and `show TASK` expose the latest `cleanup_failure` (null
after successful cleanup); history is retained. Each execute cycle's JSON
`cleanup` report includes `cleaned`, `retained`, and `skipped`, where each skipped
entry contains `task_id` and its specific, non-secret `reason`. A task-local
cleanup failure leaves its data untouched (or its unfinished intent intact if
removal began), records the reason, and skips that task while continuing cleanup
and dispatch of unrelated work. Historical ownership/configuration mismatches
are skips, not fatal cycle errors. Global configuration/database or failure to
persist evidence still fails the cycle rather than hiding infrastructure errors.
Successful cleanup is not repeated after restart or policy changes.
An interrupted deletion remains an unfinished intent and **is not retried or
inferred complete**; that task is skipped and requires operator inspection.
Never delete journal or lock records to make it run again.

Execute daemon cycles also watch finished (`pr_complete`) tasks for a
repairable PR. The pass looks up the saved PR first: a merged or closed PR
(no open PR) is skipped silently and is never compared with the current
configuration, held or failed. A task whose PR is still open but whose
selection-time configuration differs from the current one is skipped without
any state change, and the cycle's single JSON document reports it under
`pr_repair.skipped` with its reason, for example
`selection config differs: cleanup, min_free_space_bytes, initial.args`. An
unreadable PR lookup is skipped the same way and asked again next cycle. Only a
genuine safety conflict on an open PR (unproved head change, changed author,
claim or process evidence) holds the task, with a specific `PR repair held: ...`
reason listed under `pr_repair.held`. One task's problem never stops the rest of the pass,
reconciliation or dispatch.

A PR head may legitimately advance through GitHub `update-branch` or manual
merges of the configured base. Luthor accepts only a first-parent extension of
the recorded PR tip made entirely of two-parent merges whose second parents
are on that base. After fresh PR identity, claim, ownership and exit-receipt
verification, it fetches the head and base into task-private refs and
fast-forwards the clean, verified task worktree. The operator checkout's base
branch is not changed. `pr_base_merge_adopted` records the lineage and
`pr_head_verified` records the current PR proof; original completion evidence
is preserved. Foreign commits and non-base merges stay held; dirty worktrees
are never fast-forwarded.

Execute cycles also restore open-PR tasks held with `exit PR present`,
`PR repair held: PR, process, receipt or ownership evidence did not verify`, or
`PR repair held: worktree identity or tip did not verify`. Restoration requires
fresh matching PR and claim evidence, accounted attempts, verified receipts,
and an owned worktree at the matching or safely adoptable tip. An operator
fast-forward or an interrupted adoption can therefore return to `pr_complete`
and normal bounded repair. An old `exit PR present` without a saved completion
proof requires an exact local/PR tip match; it cannot adopt an unrecorded remote
head. Restoration appends `pr_hold_restored` and reports the task under
`pr_repair.restored`; unavailable evidence is reported under
`pr_repair.restore_retained`. Live reservations, unfinished attempts, cleanup
intents, semantic-conflict holds and incompatible/amended sessions are not
restored. No restoration launches a worker by itself.

PR check selection is per mapping: omit `required_checks` to retain GitHub's
`isRequired`, supply an exact names array to gate an unprotected repository,
or use `{"kind":"all"}` for all head checks except skipped runs. For example:

```json
"required_checks": ["ubuntu-latest (Rust 1.88.0)", "release build + source bundle"]
```

The selected failures use the same one-rerun-per-head behavior, immutable Actions
job verification, repair limit and bounded log excerpts. Missing named gates
stay pending. Status/show expose `required_checks_source` and `last_pr_health`;
`none` warns that check-based repair is effectively off because neither GitHub
nor the mapping supplies gates. Mapping changes remain selection-config drift,
not authorization to alter an existing session. See
[check-selection semantics](dev-docs/config-and-state.md#per-mapping-required-check-selection).
PR handoff acceptance is unchanged; check-based repair does not itself authorize merging.
Merging requires the separate opt-in policy described below.

Tasks that an earlier release held with exactly the reason
`PR repair validation or launch failed; inspect task evidence` are returned to
`pr_complete` automatically, before cleanup and scheduling, once a fresh
exact-identity read shows the saved PR merged or closed. The move appends a
`pr_hold_restored` evidence row with the terminal PR proof, keeps every earlier
row, and is reported under `pr_repair.restored`. Tasks with a live reservation,
an unfinished attempt, a cleanup intent, a different latest hold reason or a
PR that is still open stay held (`pr_repair.restore_retained`).

Dispatch reports `disk_space` with available bytes on the filesystem hosting
`worktree_root`, the configured minimum, and `dispatch_held`. Space below the
minimum holds scheduling without assigning an issue, creating a task, reserving
an attempt or starting a worker. Daemon polling can continue and admit work
once space recovers. Each new dispatch and initial/resume/retry preparation
rechecks space; unreadable space or invalid roots fail rather than dispatching
with unknown capacity. If the root does not exist, its nearest existing
ancestor supplies the filesystem measurement, without creating the directory.
This is a preflight guard, not a disk reservation: configure sufficient margin
for all concurrent builds, since other processes can consume space afterward.

## Inspect and control tasks

The read-only inspection commands open the local state database in read-only mode. They do not perform fresh GitHub reads. `status` reports task phases, reserved versus configured capacity, output-silence warnings, and stored evidence. `show` displays a task's attempts, worktree, events, and stored evidence. `logs` reads safe local attempt log files.

```sh
target/debug/luthor status --config /private/luthor/config.json
target/debug/luthor show TASK_ID --config /private/luthor/config.json
target/debug/luthor logs TASK_ID --config /private/luthor/config.json
target/debug/luthor logs TASK_ID --attempt ATTEMPT_ID --config /private/luthor/config.json
```

Pause writes a stop request for the task's latest attempt. Reconcile changes local state and may read GitHub to verify source or PR evidence; it does not launch a worker. For a task with no attempt, reconcile confirms observable claim completion and recovers clean worktree creation at the saved base; ambiguous source operations hold only that task. With an attempt, it checks process/receipt and PR evidence. Resume launches a new attempt and therefore requires explicit `--execute`.

```sh
target/debug/luthor pause TASK_ID --config /private/luthor/config.json
target/debug/luthor reconcile TASK_ID --config /private/luthor/config.json
target/debug/luthor reconcile TASK_ID --attempt ATTEMPT_ID --config /private/luthor/config.json
target/debug/luthor resume TASK_ID --config /private/luthor/config.json --execute
```

Use pause only when you intend to stop that task. Reconciliation can leave a task held when the available evidence is incomplete or conflicting; inspect `status` and `show` before deciding what to do next. Do not interpret silence as proof of a hung process: status warns after five minutes without observed output and does not kill the worker.

## Troubleshooting

- **`dispatch held: pass --execute to authorize GitHub writes`**: no dispatch occurred. Add `--execute` only when an authorized live action is intended.
- **`resume held: pass --execute to authorize worker launch`**: no worker launched. Use the explicit flag only after inspecting the paused task and its evidence.
- **A task is held or capacity remains reserved**: inspect `status` and `show`, then run `reconcile` if you intend to refresh evidence. Do not delete state or retry based on process absence alone.
- **Discovery returns no candidates**: check Project membership, issue state, marker type and exact value, assignees, milestone, source repositories, and repository mappings.
- **GitHub reads or identity checks fail**: verify `gh` is installed and the authenticated account has the required access and matches the configured identity. Luthor does not define credential fallbacks.
- **Push or PR verification fails**: check SSH access for the configured `push_remote`, allowed head repository, author, base branch, and exact issue link. An agent exit or PR claim is not sufficient evidence.
- **Offline Cargo commands cannot resolve dependencies**: the locked dependencies are not cached locally. Populate Cargo's cache through your approved environment before repeating the commands.

For state layout, migrations, and configuration details, see [Configuration and state](dev-docs/config-and-state.md). For what validation has and has not established, see [WP05 validation](dev-docs/wp05-validation.md).


### Recover an aborted supervisor dispatch

The coordinator's handshake pipe is also the cancellation token. A supervisor
that observes cancellation before spawning does not start a child. If the READY
pipe closes after an unreleased worker shim was spawned, the supervisor closes
its gate and reaps it; it never releases the worker. A closed READY pipe is an
error, not a stdout panic. The private `ATTEMPT.dispatch-aborted.json` terminal
marker records the exact launch plan, supervisor identity and, when present,
the stopped shim identity. Handshake deadlines are unchanged.

```sh
luthor recover TASK --attempt ATTEMPT --config /private/luthor.json --actor acoliver --reason 'Supervisor dispatch cancelled before worker release' --execute
```

Recovery requires the latest held reservation, an exclusive worktree owner,
matching private plan/marker/child records, the same boot session, and absence of
each recorded PID and process group. Any worker-release intent, contradictory
execution evidence, receipt, unsafe marker, changed claim/worktree or uncertain
PR lookup keeps the task held. With fresh claim and **absent PR** evidence,
recovery audits `dispatch_abort_recovered`, completes the attempt as
`dispatch_aborted`, releases capacity and returns the task to `attention`.
It does not invent a worker receipt or start a worker. This marker cannot repair
old failed dispatches that did not record it; they remain held without proof.

An explicit `retry` may then continue the same task/session/worktree with a new
attempt and config revision, rechecking the exact audited marker, process
absence, claim, PR absence, worker pin and capacity. An aborted dispatch does
not qualify for natural-exit automatic retry or `--revalidate-terminal-exit`.
Missing-receipt recovery for workers that actually ran retains its existing
held/PR-completion behavior.

### Continue after a natural worker exit

`retry` is an explicit authorization for one existing task in `attention`, naming
its latest completed, released attempt. Except for the audited dispatch abort
described above, it requires a natural exit with no signal
or stop intent, rechecks the private receipt and registered process absence, then
reads the source claim and exhaustive open-PR list again. It never reassigns the
issue or creates another task or worktree. Startup reconciliation alone never
authorizes retry; the separate policy below must be enabled for execute-cycle
automation.

```sh
luthor retry TASK --attempt PREVIOUS_ATTEMPT --config /private/luthor.json --config-revision corrected-budget-512 --actor acoliver --reason 'Correct unsupported worker budget from 1024 to 512' --execute
```

The new attempt uses the current private config's `resume` executable and argv.
Its session, task branch, worktree identity, source rules, mappings, assignment
login, state/worktree roots, and capacity must remain unchanged. Only the two
worker command templates may change. Give the new attempt a revision different
from the previous attempt's revision. Keep both current templates valid, including
`--max-tool-calls 512` instead of `1024`; `-1` also means unlimited. The original
selection and previous attempts remain unchanged, including an old unsupported
argument. The private `retry_authorized` evidence records the actor, reason,
previous plan/config, new plan/config, fresh source/claim and PR absence, and reservation ID in the
same transaction as the new launch intent and reservation.

`--execute` is required. Missing or uncertain receipts/processes, pending stops,
PR errors/ambiguity/presence, claim or identity drift, conflicting worktrees,
invalid configuration, and unavailable capacity refuse the continuation. An
uncertain launch keeps its new reservation and must be reconciled; running the
same command again does not launch another worker. No SQLite edits or worker
wrappers are needed.

Normal `resume` retains its original stopped-attempt and selection-configuration
requirements. It does not adopt a retry's changed revision. A revised retry that
is subsequently paused cannot be resumed through that original-config path.
Another natural exit may be explicitly retried with a different revision and
fresh proof. Automatic continuation is disabled unless explicitly configured
below. The process-absence check covers
registered processes and tracked descendants, not deliberately untracked escaped
children. A reused PID or boot change fails closed.

For a historical Darwin attempt that failed during native CLI startup on
`--max-tool-calls 1024`, add `--revalidate-terminal-exit` immediately before
`--execute`. This requires the exact reconciled exit-code-2 receipt, the matching
single native startup JSON diagnostic, empty stderr, consistent original
registrations, no tracked descendants, and current absence of both recorded PIDs
and groups. The audit records this separate terminal proof without changing the
old boot identity. Ordinary historical exits stay held because escaped runtime
workers cannot be excluded by a receipt and group probes alone. See
[terminal startup revalidation](dev-docs/config-and-state.md#darwin-boot-identity-and-terminal-startup-revalidation)
for the proof and refusal boundaries.

### Opt-in bounded transient retries

To authorize automatic **task-attempt** continuation during `daemon --execute`
or `cycle --execute`, set the optional top-level configuration field:

```json
"retry_policy": {
  "kind": "bounded",
  "max_attempts": 3,
  "backoff_secs": 90,
  "classes": ["rate_limit", "transient_server", "connectivity", "turn_budget"]
}
```

Omission or `{"kind":"disabled"}` preserves operator-only retry. This is not
provider-request replay: the worker must have exited naturally, reached
`attention`, and released its reservation. The total attempt limit includes the
initial attempt, manual retries, resumes, and automatic retries. Backoff starts
when the daemon first durably classifies that attempt; repeated polls and daemon
restarts cannot reset it. Exhaustion leaves the task in attention, without a new
reservation. Manual `retry` remains a separate explicit operator authorization.

The worker must emit one terminal JSON object on stdout with the matching
`session_id`, `status:"error"`, and a typed `error.code`; transport errors must
agree with exit code 5. `model-rate-limit`, `model-transient-server`, and
`model-connectivity` map to the corresponding transport classes. The separate
opt-in `turn_budget` class matches `turn-budget` (per-turn round, assistant-byte,
tool-argument and tool-output budgets) and `turn-time-exhausted` (turn deadline),
both with exit code 6. A normal tool-call cap can instead finish successfully;
`budget_exhausted` on a successful envelope does not authorize a retry. Startup
usage errors such as `max-tool-calls` do not match. Luthor does **not** infer a
retry class from stderr, summaries, stack traces, HTTP numbers, or substrings such
as `body read failed` or `timeout`. Quota/permanent failures, unknown/malformed
output, stop/signal exits, and explicit terminal verdicts never auto-retry. A
successful worker exit without a matching PR needs human review, including
workers that ask for a missing handoff. It is reported as `needs_human_input`,
not as a transport failure.

Automation calls the same audited retry path as the operator command: fresh
GitHub identity, source/claim and PR-absence checks; exact task/session/worktree
identity; receipt and registered-process absence; worktree ownership and
capacity. A busy worktree lock (including one retained by a detached gate after
natural worker exit) reports `worktree_busy` and waits for a later execute poll;
it consumes no attempt ID or reservation and writes no retry-request/failure
evidence. Backoff is not reset. The same exclusive owner is retained through
verification and dispatch once acquired; unavailable or invalid ownership still
fails closed. It uses the configured resume command with a fresh attempt ID and an
automatic revision suffix. Include `{attempt.id}` in the resume prompt so each
continuation is distinct. No new assignment, task, or worktree is created; no
historical terminal-exit exception is enabled. Safety or upstream errors fail
the pass with an audited failure, not a fallback. An uncertain launch retains
its reservation and is never replayed automatically.

The current policy intentionally applies to existing attention tasks without
changing their immutable selection configuration. `worker_outcome` evidence
persists classification and observation time; `automatic_retry_requested`
records the exact policy, prior attempt count, new attempt ID and attribution.
`retry_authorized` retains the existing transactional launch/claim/PR audit.
Execute reports include `automatic_retries` with classification and decisions
such as `backoff`, `capacity_full`, `worktree_busy`, `exhausted`, and
`needs_human_input`. Cached
`status` and `show` expose the latest `worker_outcome`; those views, previews,
plain reconciliation, and stopped-task resume do not initiate retries.

## Interrupted selection and claim recovery

Execute-cycle startup reconciles tasks interrupted before their first attempt.
An existing `claim_verified` record is confirmed, not reported as an unfinished
claim. If completion was not recorded, exact assignment to `assignment_login`,
matching issue identity, ready marker and Project membership establish it without
another GitHub assignment write. An unobservable claim stays held.

An unfinished worktree intent is recovered only for the mapped checkout, task
branch, exact saved base commit and a clean registered Git worktree. An absent
path is created using that same pinned commit, without fetching a different base.
Partial paths, foreign repositories, changed branches/tips and local changes are
left untouched and held. Historical intents without a pinned base cannot be
adopted automatically. Reconcile never launches; the execute scheduler may launch
an attempt-free verified task after fresh claim, PR-absence and account checks.
An already-recorded launch intent is never replayed automatically.

Source holds keep their own capacity slot but no longer stop dispatch to free
slots. Local state errors, unresolved stops and attempt reconciliation errors
still block globally. To release an interrupted selection, use:

```text
luthor abandon TASK --config PATH --actor LOGIN --reason TEXT --execute
```

This accepts `preparing`/`selected`/`claimed` tasks with no attempt, or a launch
intent proven never dispatched by contemporaneous preparation ownership and the
strict continuation proof (not merely absent dispatch rows). A busy
worktree owner or any potentially running attempt is refused. The audit records
actor, reason and previous phase; GitHub, worktree files and branches are not
changed. The operator can unassign the issue separately and let discovery select
a new task. The abandoned task's complete history remains available.

## Historical attempts without sufficient proof

Historical unknown attempts remain **permanently held** under the current
contract. A free lock today, absent processes, empty logs, repaired storage,
issue closure or PR merge cannot prove that an old worker never started or
exited safely. In particular, a pre-owner-protocol launch cannot be adopted, and
an old Broken-pipe dispatch without terminal abort/exit proof cannot be relabeled
as a dispatch abort. Do not use continuation, amendment, retry, resume, abandon,
cleanup or manual reservation edits to bypass either hold.

New initial launches record attempt-bound preparation ownership atomically with
their reservation and launch intent. Undispatched continuation/amendment and
abandonment require that contemporaneous proof and its current owner binding;
schema migration never backfills it for old attempts. Attempt-free selections
and already verified terminal attempts keep their existing recovery contracts.

If no authorized recovery proof exists, the only operational alternative is
external isolation and decommissioning of the **whole queue**, preserving its
unknown history and reserved capacity. This is not a reservation-release command.
A replacement queue must have externally enforced non-overlapping ownership and
resource accounting; fresh state alone is not authority to restart unknown work.
See [the permanent-hold and controlled queue-retirement contract](dev-docs/issue27-historical-hold.md)
for prerequisites, explicit refusals and hermetic regression coverage. This does
not authorize modifying or retrying either live attempt from issue #27.

## Worker build pins

The first launch intent records the resolved worker executable path and SHA-256
of its bytes. `status` and `show` expose this as `worker_build` (including
`pin_status`). New selections capture the current initial template's build;
resume, retry and PR repair keep the session's original build even when the
operator updates executable templates. Authorized argv/prompt corrections do
not change the pin. A worker-reported top-level JSON `session_state_version`
is saved as `session_state_format` evidence and shown as
`reported_session_state_version`; Luthor never guesses a format from a binary.

Keep old executables at their recorded paths until their tasks finish. Luthor
checks the hash before dispatch and at the worker gate. A missing or replaced
build holds continuation rather than trying a new binary. Existing dispatched
sessions whose plans predate build pins show `legacy_unrecorded` and cannot
silently acquire the current binary's identity. There is no automatic copying,
rebuilding, repinning or session migration; restore the pinned build or use a
separately authorized session migration workflow. Pin adoption below records
operator-attested provenance; it does not convert session formats.

### Adopt a build pin for a legacy session

For a non-running `legacy_unrecorded` task whose lifetime build is known:

```sh
luthor adopt-pin TASK --config PATH --executable PATH --sha256 HEX \
  --actor LOGIN --reason 'Evidence that this executable was used throughout the task'
# Review the single JSON preview, then repeat with --execute to commit.
```

The executable argument must **exactly equal** the initial or resume executable
in the task's saved selection snapshot, not merely today's config or an alias
of the same file. Relative paths/PATH names resolve from the saved task worktree
as at ordinary build selection. The file must be executable and hash to the
supplied 64-hex-digit SHA-256 now. The operator attests its historical use;
Luthor cannot infer past bytes from today's file. `--actor` and a nonempty
`--reason` are required and recorded, not a GitHub write or reassignment.

Preview is read-only; `--execute` rechecks all requirements under the coordinator
and worktree-owner locks. All attempts must have reconciled exits and released
reservations: reconcile first, including a held task whose worker may still be
running. Already-recorded or adopted pins are refused; no repinning is supported.
The command emits exactly one JSON document, including on refusal (nonzero exit).

Execution appends `worker_pin_adopted` evidence with actor, reason, supplied path,
resolved build path, hash, time, historical attempt IDs and phase transition.
Historical plans, argv, dispatch artifacts and exits remain unchanged. `status`
and `show` then report `pinned`, and future resume, retry and PR repair use that
build and the same dispatch/worker-gate hash checks as a launch-recorded pin.
Only a hold specifically caused by the unrecorded legacy pin is restored to its
prior continuation phase; unrelated safety holds and incompatible-session checks
remain in force. Adoption launches nothing and changes nothing on GitHub.
Daemon auto-adoption is not enabled; adoption always requires this explicit
operator action.

A naturally failing worker's structured `error.message` reporting
`unsupported state version N`, or `error.code` of `session_incompatible` /
`session-incompatible`, is classified as `session_incompatible`. Equivalent
plain `error:` / `corrupt session state:` diagnostics are recognized too.
Durable evidence names the pinned and attempted builds and the rejected version
when reported. The task stays held after process/receipt accounting releases
its slot; no PR completion or new retry is authorized for that session.

## Rust quality gates

From the repository root, use **`cargo xtask ci`** for the same offline, locked
policy enforced on Ubuntu and macOS. See [the xtask guide](xtask/README.md) for
the compatible toolchain, deliberate dependency fetch, absolute workspace-local
Cargo/fixture directories, strict measurements and executed contracts.
All structural limits apply to existing and new code, with no exceptions;
refactor code that exceeds them.
Keep cwd stable for Unix stop-socket fixtures; do not use an external `/tmp`
workaround or weaken production path validation.

### PR provenance and issue-closing references

Every initial worker launch, stopped-session resume and audited natural-exit
retry requires both references on separate complete lines in the PR body:

```text
Tracker-Issue: https://github.com/OWNER/REPO/issues/N
Fixes #N
```

The tracker repository and issue number come from the verified task selection.
When the tracker and code repositories differ, the closing line is instead
`Fixes OWNER/REPO#N`, referring to the **tracker** repository. The exact
`Tracker-Issue` line remains required for supervisor provenance matching;
`Fixes` alone cannot satisfy it. Author, claim, base and head checks are unchanged.

GitHub closing keywords in PR bodies only automatically close issues when the
PR targets the code repository's **default branch**. The maintained Luthor base
is `work/luthor-issue-to-pr-daemon`, while this repository's default is
`bootstrap/luthor-base`. These references improve linking but cannot by
themselves guarantee closure after merge to the maintained base. With the
mapping's merge policy enabled, Luthor confirms its exact-head merge, then
closes a still-open tracker issue with a comment naming the PR and merged head,
and journals the closure. With merging disabled, operators must still verify
merged delivery and close remaining issues. Neither path changes the configured
base, repository default, branch protection or safety checks.

### Optional merging of verified PRs

Merging remains **off by default**. A mapping can explicitly opt in with
`"merge": {"kind":"enabled","method":"squash"}`; merge checks default to all
visible checks, and drafts are excluded. The execute daemon merges only the
saved verified PR head when fresh checks pass or skip and GitHub reports it
MERGEABLE and CLEAN. For an owned green BEHIND PR, it first requests a
head-pinned **merge** of the base, never a rebase or force push, and waits for
existing lineage verification and checks on the new head. Conflicts go through
the existing repair path. It journals updates, merges and tracker closure;
uncertain writes are never retried. A merge-step failure for one task (an
unreadable or changed PR, an uncertain write, a refused closure) is journaled
as `pr_merge_error`, reported under `pr_merge.waiting` with its reason and
holds only that task: the cycle still exits 0 and schedules other work. A
verified PR that was merged outside Luthor after its head advanced is recorded
as delivered (`pr_merged` retirement, reported under `pr_merge.delivered`) only
when its identity is unchanged and the head moved purely by merges of the
configured base from the saved verified head; any other change holds the
task. Luthor writes no merge evidence for a merge it did not make. See
[merge policy and recovery](dev-docs/config-and-state.md#opt-in-verified-head-merging-issue-40)
for named/GitHub-required check rules, draft promotion, evidence and recovery.
This does not enable merging in any operator config or bypass hosted CI or
branch protection.
