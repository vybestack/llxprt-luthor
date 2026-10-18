# Configuration and state contract

Configuration is a JSON object with `state_root`, `worktree_root`, positive `capacity`, an explicit non-secret `assignment_login` (the configured issue assignee, independent of the allowed PR author), non-empty `sources` and `mappings`, and `initial`/`resume` command templates. Each source has a Project ID, repository names (`owner/repo`), one `ready_marker`, and optional exact `milestone`. A marker is either `{ "kind": "label", "name": "..." }` or `{ "kind": "project_field", "name": "...", "value": "..." }`. Each mapping identifies tracker and code repositories, checkout path, base branch, push remote, allowed PR head repository, and allowed PR author. The allowed PR head repository may be either the in-repository tracker or code repository (including when they are the same repository) or an authorized fork; it must use `owner/repo` format. Commands contain an executable path and argv array. Supported literal substitutions are `{task.issue_number}`, `{task.repository}`, `{task.issue_url}`, `{task.id}`, `{attempt.id}` and `{worktree}`. These are argument templates, not shell: argv is exec'd directly, so shell metacharacters in an argument are ordinary text. Luthor does not validate worker flags, flag/value pairing or flag values; the worker validates its own command line and Luthor does not mirror its rules. Template syntax (known variables only, balanced braces), the executable path and credential-bearing arguments are still checked. Keep credentials in the external credential store/environment, never configuration or argv.

Synthetic example (all paths, IDs and names are illustrative; no key is present):

```json
{
  "state_root": "/private/luthor/state",
  "worktree_root": "/private/luthor/worktrees",
  "capacity": 1,
  "assignment_login": "example-agent",
  "sources": [{"project_id":"PVT_SAMPLE","repositories":["example/tracker"],"ready_marker":{"kind":"label","name":"daemon-ready"},"milestone":"v1"}],
  "mappings": [{"tracker_repository":"example/tracker","code_repository":"example/code","checkout":"/src/example-code","base_branch":"main","push_remote":"git@github-acoliver:example/code.git","allowed_pr_head_repository":"example/code-fork","allowed_pr_author":"acoliver"}],
  "initial": {"executable":"/usr/local/bin/llxprt-code-rs","args":["--session","{task.id}","--cwd","{worktree}","-p","Work on {task.issue_url} (task {task.id}, attempt {attempt.id})"]},
  "resume": {"executable":"/usr/local/bin/llxprt-code-rs","args":["--session","{task.id}","--cwd","{worktree}","-p","Continue task {task.id}, attempt {attempt.id}, from {task.issue_url}"]}
}
```

### Worktree root placement (issue #33)

Choose a **short, dedicated absolute `worktree_root` outside the Luthor checkout
and any other Cargo or similar build-tool workspace**. Keep the mapped checkout,
private `state_root` and worker worktrees separate. For example, a synthetic
layout could use `/src/luthor` for Luthor, `/src/example-code` for the mapped
checkout, `/private/luthor/state` for state and `/private/lw` for `worktree_root`.
The paths are illustrative, not instructions to modify a live installation.
Validate access controls and the actual ancestor directories before use.

Cargo walks up from a package to find its workspace. Git worktree isolation
does not stop that search. A target Rust package with no `[workspace]` table,
placed at `<luthor checkout>/tmp/worktrees/task-…`, can discover Luthor's
`Cargo.toml` (`[workspace] members = ["xtask"]`) and fail with “current package
believes it's in a workspace when it's not.” An ignored `tmp/` directory or a
Git worktree `.git` file is not a Cargo workspace boundary. The same risk applies
to an unrelated enclosing workspace, not only Luthor's own manifest. Do not
modify a target repository's manifest just to compensate for its launch location.

Before queueing a Rust repository, inspect the proposed root's ancestors for
`Cargo.toml` and other build-tool configuration. In a disposable representative
worktree at that location, run `cargo locate-project --workspace` and
`cargo metadata --offline --locked --no-deps --format-version 1` with the intended
worker environment and an existing lockfile. Confirm they identify the target's
own workspace, not an ancestor's. These checks do not replace the target's build
and test gates. Luthor's current config validation and worktree preflight check
Git/path identity, **not** Cargo workspace membership or other build tools;
a successful preflight alone does not establish a safe build environment.

Keep the root short as well as separate: the generated `task-<id>` component,
the worker's `tmp/` tree, and extracted source bundles all add path length.
A root that fits Luthor's own stop-socket check can still yield worker test paths
longer than the platform's Unix-domain socket limit. There is no universal safe
root length independent of target fixture suffixes. Verify the target's fixtures;
where supported, bind/connect sockets through bounded relative paths while
keeping cwd stable, rather than changing a timeout or weakening an assertion.
See [Luthor's confined fixture guidance](../xtask/README.md) for local tests.

Placement is an operator configuration contract, not an environment-inheritance
fix. In particular, an ambient `CARGO_HOME` that happens to stop Cargo's ancestor
search masked the original failure; do not rely on it. This guidance adds no
implicit worker environment overrides. If an installation deliberately chooses
an environment-based solution instead, make it explicit in the configured
worker command template/wrapper (for both initial and resume commands) and test
that environment; exporting a variable in the daemon's launching shell is not a
portable placement policy.

Apply a corrected root **before selecting new tasks/queues**. A same-task retry
may change command templates, but cannot change the saved `worktree_root`;
resume also retains the original worktree. Editing the current config does not
relocate existing tasks. Do not move a live worktree, rewrite saved state, or
remove ownership locks to force adoption of a new location. Inspect and handle
existing tasks through the documented operator lifecycle; this change neither
migrates them nor edits any live configuration.

### Optional source dependency ordering

A source may include `dependencies`, an array of rules with an `issue` reference,
non-empty `blocked_by` references, and optional `allow_closed_unmerged_prs`
(default false). References are `{"repository":"owner/repo","number":123}`;
GitHub determines whether the number names an issue or a PR. Only targets must
belong to the source; prerequisites may be in any accessible repository.

All direct prerequisites are read live. Open issues/PRs block; closed issues and
merged PRs satisfy; closed-unmerged PRs block unless that rule explicitly opts
in. No Project membership, readiness marker, assignee, or Luthor task completion
is required for a prerequisite. Config validation rejects invalid references,
empty/duplicate rules or prerequisites, self-dependencies, cross-source
conflicts/omissions and cycles (including across repositories). Overlapping
sources must carry the same rules for their shared repositories.

No SQLite migration is needed. Rules are included in the existing source and
effective-configuration snapshots. Omitted/empty rules preserve old snapshot
serialization and old scheduling behavior. Live dependency rechecks run during
selection, before creating a new task, and in claim/prelaunch verification.
Blocked selections create no task or reservation. A prerequisite reopening after
selection holds the existing dispatch rather than authorizing assignment or
launch from stale evidence. Existing holds/reservations are never cleared by a
dependency read. See [dependency-aware scheduling](../README.md#dependency-aware-scheduling)
for JSON examples and waiting/report semantics.

### Optional task-attempt retry policy

`retry_policy` defaults to `{"kind":"disabled"}`. Opt in with
`{"kind":"bounded","max_attempts":3,"backoff_secs":90,"classes":["rate_limit","transient_server","connectivity","turn_budget"]}`.
`max_attempts` is an unsigned total task-attempt bound (at least two), not a
count of provider HTTP replays. `backoff_secs` must be a positive unsigned delay;
classes must be nonempty, distinct, and chosen from `rate_limit`,
`transient_server`, `connectivity`, and `turn_budget`. The first three match
`model-rate-limit`, `model-transient-server`, and `model-connectivity` with exit
code 5. `turn_budget` matches only typed `turn-budget` and `turn-time-exhausted`
errors with exit code 6 and the saved session ID. `turn-budget` covers per-turn
round/assistant-byte/tool-argument/tool-output limits; `turn-time-exhausted` covers
the turn deadline. The ordinary tool-call cap may produce a successful summary
instead of an error: neither that success nor its `budget_exhausted` field, nor
startup usage errors (`max-tool-calls`, exit 2), authorize continuation. Unknown
fields/classes and incomplete policies fail configuration validation. Policy is operational, like cleanup: it is not added
to the immutable selection snapshot, so enabling it can authorize existing
attention tasks. Its exact value is recorded in each automatic retry request.

No schema migration is required. The existing append-only attempt history
provides the total count, including operator continuations. `worker_outcome`
evidence stores a bounded classification enum and first-observation Unix time,
never worker text. It is reused after restart and checked against the current
private stdout envelope. `automatic_retry_requested` links that observation,
policy and previous count to the new attempt; failures have bounded
`automatic_retry_failed` evidence. Existing `retry_authorized` evidence and
transactional reservation/launch intent are unchanged. No direct state edits
are needed to enable policy or continue a task. A held worktree owner delays
automation with `worktree_busy`, without a request/failure audit, new attempt ID,
or reservation. Later execute polls acquire exclusive ownership before journaling
a request and retain it through the existing verification/dispatch path. Missing
or invalid ownership is still an error; a lock is never stolen.

Retry task identity comparison excludes source discovery rules and mappings for
other tracker repositories. It still compares the selected repository's entire
mapping and all task-affecting snapshot fields. The same comparison is applied
to retry authorization, launch-intent validation and supervisor attempt selection;
none of these paths rewrite the original selection or prior attempt evidence.
Expected admission refusals are recorded as `scheduling_admission_failed` with
`task_id`, `attempt_id`, `check`, a bounded reason and `differing_fields`. They do
not create an attempt or bypass fresh claim/worktree/exit checks. Unexpected
storage, malformed-source and uncertain-dispatch errors still abort the cycle.

See [bounded transient retries](../README.md#opt-in-bounded-transient-retries)
for the worker JSON/exit agreement, nonretryable and human-review outcomes,
execute-only boundaries, and safety checks. The worker's typed transport
contract was verified at `vybestack/llxprt-code-rs` commit
`9cc8677aa8270d810a32fcafb4a2c0f52778175e` (`src/envelope.rs`,
`src/agent/error.rs`, `src/adapter/error.rs`, `src/transport.rs`, and
`tests/error_contracts.rs`). The turn-budget/deadline and normal tool-call-cap
behavior was verified at worker commit
`d7c40c11340bab085a804339b99e3906428c2aa6` (`src/agent.rs`,
`src/agent/deadline.rs`, `src/agent/tool_round.rs`, `src/envelope.rs`); no log
regex or private worker wrapper is involved.

### Per-mapping required-check selection

Optional `required_checks` on each mapping chooses the check gates used by
PR health, failed-job reruns and repair. It does not change branch protection:

- Omitted (or `{"kind":"github"}`): use GitHub's `isRequired`, unchanged.
- `["ubuntu-latest (Rust 1.88.0)", "release build + source bundle"]`: use exactly
  these check-run names or legacy status-context names, even if GitHub marks
  none required. Names are case-sensitive, unique, non-blank ASCII strings of
  at most 128 bytes; an empty array is invalid. A named check not yet present
  stays pending, rather than being treated as green.
- `{"kind":"all"}`: use every check run and status context on the PR head,
  excluding skipped check runs. Pending checks wait; skipped checks do not
  cause reruns or job-detail reads. Existing success/neutral classifications
  are unchanged.

The selected set replaces (rather than unions with) GitHub's required set.
GraphQL pagination, latest check-run selection, exact PR/head and immutable
Actions job/attempt checks are unchanged. Each failed Actions run is rerun once
per verified head; a repeated failure supplies bounded log excerpts to repair.
Failed non-Actions checks require a person, as before. This setting never
bypasses GitHub's own merge requirements.

`pr_health` evidence records `required_checks_source` as `github`,
`configured_list`, `configured_all`, or `none`. **`none` means GitHub returned
no required checks and no mapping override was configured: check-based repair
is effectively off**, though merge conflicts can still trigger repair. Status
and show expose the source and cached `last_pr_health` (including PR ID/head,
selected failed checks/jobs and pending state); those cached health views make no fresh
GitHub health read (`status` may separately discover dependency waiting). Before any health observation the source is null; historical
health evidence without a source reports `unknown`, never inferred `none`.

This field is included in candidate mappings and effective configuration
snapshots. Old mappings deserialize to GitHub selection, and the default is
omitted when serializing to preserve existing snapshot bytes. Check-policy
changes count as mapping drift: existing open-PR tasks selected with a different
policy are skipped, not silently repinned. Configure it before selecting tasks;
this change provides no migration of existing task authorization.

`pr_complete` still means a verified open-PR handoff, not green CI or a merge:
draft/red/pending PR acceptance is unchanged. The separate opt-in merge policy
for issue #40 uses the same required-check selection syntax, but requires
SUCCESS or SKIPPED conclusions and fresh MERGEABLE/CLEAN proof before writing.
See [verified-head merging](#opt-in-verified-head-merging-issue-40).

### Cleanup and available-space policy

Optional top-level fields:

- `cleanup` defaults to `{"kind":"disabled"}`. Opt in with
  `{"kind":"artifacts","paths":["target","tmp/build"]}` or
  `{"kind":"worktree"}`. Artifact paths must be relative, non-empty,
  non-overlapping and outside `.git`; runtime checks require ignored,
  untracked paths and refuse symlinks, nested repositories, special files
  and filesystem boundaries.
- `min_free_space_bytes` defaults to `0`; for example `53687091200` holds
  dispatch below 50 GiB available to the current user on `worktree_root`'s
  filesystem. Equality permits dispatch. Measurement failures fail closed.

Both fields are included in new effective configuration snapshots. Older
JSON snapshots deserialize to the defaults without altering saved rows.
Cleanup uses the **current** policy for existing terminal tasks, but requires
unchanged task roots and mappings. Launch continuations retain their existing
snapshot/config identity rules. Changing cleanup/space policy does not authorize
an otherwise-invalid retry or resume.

Execute daemon and explicit dispatch perform cleanup before space preflight;
read-only/preview commands never remove files. Only `pr_complete` tasks with
completed released attempts and fresh terminal proof for their exact saved PR
are eligible. The ownership lock, process/receipt proof, worktree identity,
PR repository/number/ID/base/head/author/tracker linkage and absence of other
matching open PRs are rechecked. Whole-worktree removal additionally requires
no unsaved files and the current PR head tip. Git branches, fetched base pins,
private state, logs and locks remain. Telemetry-loss and historical boot proof
uncertainty refuse cleanup rather than inferring quiescence.

The schema is unchanged. A `cleanup` intent records policy, targets, immutable
worktree identity, fresh merged/closed PR evidence and observation time before
any deletion. `cleanup_removed` records each removed absolute path;
`cleanup_complete` prevents repeat cleanup across restarts and policy changes.
`cleanup_failed` preserves a specific non-secret reason for each skipped task.
Task-local failures do not stop cleanup or unrelated scheduling; the cycle JSON
includes `cleanup.skipped` entries with `task_id` and `reason`, and status/show
include `cleanup_failure`. A successful cleanup clears the displayed failure
without erasing history. Whole-worktree cleanup may remove ignored, entirely
untracked fixture repositories under confined `tmp/`; tracked submodules and
all other nested repositories (including artifact-only targets) remain refused.
An unfinished intent requires inspection, never automatic retries. `show` displays removed paths and
completion/failure events without exposing prompts or config. See the
[operator cleanup guide](../README.md#terminal-task-cleanup-and-disk-space-dispatch-holds)
for operational boundaries.

### Remote base for new task worktrees

Before creating a new task worktree, Luthor fetches `refs/heads/<base_branch>`
from the mapping's `push_remote` (a configured remote name or explicit URL).
The fetched tip is pinned in `refs/luthor/task-bases/<task_id>` and its commit
ID is persisted as `base_commit` in the task's `worktree_create` intent and
`worktree_created` evidence. Worktree creation uses that commit ID, not the
local branch, `FETCH_HEAD`, or a shared remote-tracking ref. The checkout's
local base branch is neither required nor moved.

A failed fetch (including a missing remote base) holds the task with
`remote base fetch failed`; no task branch, worktree, or worker launch is
created and there is no stale-ref fallback. Task assignment may already have
been verified, so a held task still requires operator inspection. Reusing or
inspecting an existing worktree never fetches or changes its recorded base.
Older persisted worktrees without `base_commit` remain inspectable under the
existing identity and ancestry checks; all newly created worktrees record it.

The initial launch and every continuation use the task ID as the worker session ID. Each continuation gets a new attempt ID in its prompt, while keeping the issue URL and verified worktree. The supervisor adds its mandatory issue-to-PR instructions to each rendered prompt and rejects a resume that changes the session, worktree, or prompt from its predecessor. These command templates are parsed by `Config::from_json`, rendered as argv (without a shell), then checked by the production launch preparation path.

## Local operator commands

Set `CONFIG` to the validated configuration file. `discover` reads configured GitHub Projects and prints eligible candidates. `status`, `show`, and `logs` read local state and do not write to GitHub:

```sh
CONFIG=/private/luthor/config.json
luthor discover --config "$CONFIG"
luthor status --config "$CONFIG"
luthor show task-7f3a --config "$CONFIG"
luthor logs task-7f3a --attempt attempt-01 --config "$CONFIG"
```

Execution controls require explicit `--execute` where applicable. Dispatch claims an eligible issue, creates its task/worktree and launches the worker; these steps can write to GitHub and local state. Daemon preview reads candidates; adding `--execute` authorizes scheduling and GitHub writes. Without `--once`, daemon polls every 30 seconds. Resume launches a new attempt in the existing session and worktree. Pause records and sends a stop request. Reconcile inspects attempt/process evidence and may query GitHub for pull-request evidence; it updates local state.

```sh
luthor dispatch --config "$CONFIG" --repository example/tracker --issue 7 --config-revision local --execute
luthor daemon --config "$CONFIG" --config-revision local --repository example/tracker --issues 7,8 --once
luthor daemon --config "$CONFIG" --config-revision local --repository example/tracker --issues 7,8 --once --execute
luthor resume task-7f3a --config "$CONFIG" --execute
luthor pause task-7f3a --config "$CONFIG"
luthor reconcile task-7f3a --attempt attempt-01 --config "$CONFIG"
luthor reconcile task-7f3a --config "$CONFIG"
```

Dispatch without `--execute` stops with an authorization message and makes no scheduling writes. Resume without it likewise does not launch. `pause` and `reconcile` are state-changing controls, so use them only when you intend to affect the task lifecycle. `reconcile TASK --config` also handles source-intent reconciliation when the task has no attempt; with an attempt, it reconciles process and pull-request evidence.

The marker name/value and optional milestone are exact selectors. Omitting milestone removes only that eligibility check. State uses SQLite schema version 8 with task, attempt, intent, reservation, evidence, metadata, launch-preparation, PR-poll and queue-capacity audit tables. New initial preparations capture attempt-bound worktree ownership in the same transaction as the attempt, reservation and original launch intent. Version 4 databases gain an empty preparation table: there is deliberately no backfill, and historical absence of dispatch records alone cannot authorize continuation or abandonment. See [issue 27's permanent-hold contract](issue27-historical-hold.md). Reservations are durable per-attempt history keyed by attempt ID, with a partial unique index allowing at most one active reservation per task. Opening a fresh database creates the current schema. Version 3 databases gain an abandoned-history-aware issue identity index transactionally; all child records, foreign keys and sequence values are preserved. Only abandoned tasks permit a replacement selection; other issue identities remain unique. Version 1 databases gain persisted capacity metadata, and version 1 or 2 databases are upgraded transactionally through the known schema versions to version 8, preserving task, attempt, intent, reservation and evidence rows. If a migration cannot complete, its schema changes and version update roll back together. Negative versions and versions above 8 are rejected. Version 6 adds PR polling metadata; version 7 adds an empty queue-level capacity audit table without rewriting task history. Version 8 adds the `permanent_holds` JSON array to that audit; existing audits gain `[]` and retain all original values. Configured capacity is persisted and must match on later opens. Creating a task stores a typed selection-evidence record in the same transaction as task identity, preserving the selected Project and item, issue URL and identity, marker, milestone, repository mapping, observation time, source, configuration revision, and a typed effective configuration snapshot (state/worktree roots, capacity, sources, mappings, and initial/resume executable and argv). The snapshot is validated before task creation and committed atomically with the task and selection evidence. Credentials are excluded by configuration validation; invalid-configuration errors do not echo supplied values or raw JSON. Duplicate task creation rolls back without leaving a second selection record. A process-level coordinator lock (where used) supplements database transactions; reservations remain authoritative and uncertain work must retain capacity.

## Audited queue capacity changes

`resize-capacity --config PATH --expected-capacity N --expected-reservations N --actor LOGIN --reason TEXT --execute`

The config supplies the **new** positive capacity; both expectations describe the
existing queue. Run `status --config PATH` first: its `capacity.limit` always comes
from persisted metadata, not the supplied config, and `capacity.reserved` reports
the current reservation count. Stop the coordinator/daemon before resizing. A
running daemon is not signalled or silently bypassed: the command fails with
`coordinator busy; try again`. Workers may survive a coordinator; an increase
can count confirmed running attempts, but unsettled launches still require
reconciliation before retrying the resize. No GitHub calls or worker launches are
part of this operation, and actor is the operator-provided audit identity, as for
other explicit operator controls.

The operation opens only an **existing** database under the coordinator lock,
using the expected old capacity through the ordinary mismatch check. Known
versions 2–8 are migrated transactionally to version 9. A pre-metadata database
(version 0/1) has no persisted capacity to verify: initialize/migrate it normally
under its original config first. Unknown versions are refused, not guessed.
There is no automatic adoption of a conflicting audit schema.

Under an immediate SQLite transaction it verifies the current reservation count
and reservation/attempt/task identities. The new limit must be different,
positive, representable by SQLite, and at least the verified reservation count;
inconsistent counts above the old limit are refused. Unlaunched reservations may
remain, but in-progress or unreconciled launch intents are refused even on an
increase. The two increase-only classifications below do not authorize decreases.
Released launches must already have their reconciled receipt/outcome or existing
explicit recovery/retirement audit; resizing never creates exit or absence proof.
Except for a confirmed running attempt, registered processes/groups must be
provably absent, and existing worktree-owner locks are acquired and held through
commit. Missing or conflicting historical owner locks are not recreated to manufacture proof. Reused PIDs and unavailable
process inspection remain fail-closed.

An **increase only** may count a confirmed running attempt without stopping or
reconciling it. The existing read-only reconciliation evidence checks must bind
its saved plan, launch/dispatch, selection, worktree, claim, child registration,
supervisor, gate release and gate send. The held task must retain its exact
active reservation and unknown outcome. Both child and supervisor must have
verified live identities and process groups; a stop intent, receipt, supervisor
error, terminal evidence or malformed/duplicate/misbound stage blocks this
classification. The persisted owner protocol must match the existing lock's
inode and nonce, and that lock must still be busy. Resizing neither takes over
nor recreates the live worker's ownership. Registrations outside this exact
attempt still require absence proof.

Read-only running observation and the immediate transaction share one continuously
held coordinator lock: no cooperative launch/reservation writer can intervene.
Workers can finish during observation, but only reconciliation can change their
reserved slots, and resizing never does so. The audit and JSON result identify
each counted running task/attempt in stable order in `running_attempts`.

An **increase only** may count a permanent historical hold without releasing it.
The task must be `held`, with exactly one initial attempt, still `launch_intended`
with unknown outcome and an active reservation, and exactly one original launch.
There must be no contemporaneous launch-preparation record (even malformed or
misbound preparation records do not qualify), terminal/recovery proof, retry or
repair authorization, or initial-branch amendment. The original dispatch
inventory must be incomplete: a complete set of supervisor-dispatch, gate-release,
owner-protocol, supervisor-ready, child-registration and gate-sent records must
instead go through reconciliation. A modern prepared launch is never classified
as historical simply because its receipt or dispatch stages are missing.

This is read-only classification of #27's unknown historical attempts for queue
arithmetic, not an assertion of worker exit or a new release protocol. All
registered-process and worktree-owner checks above still run, and unavailable
owners are not repaired. Every counted reservation stays reserved, and the audit
and CLI result name each counted task/attempt and its reason in `permanent_holds`.
No task phase, attempt outcome, launch/preparation bytes or continuation authority
changes. Decreases cannot use this exception, including decreases still above
the reservation count. The new limit must always cover **all** reservations.

One `capacity_changes` record stores actor, reason, old/new capacity, reservation
count, named `permanent_holds` and `running_attempts`, sequence and UTC timestamp
atomically with the persisted limit update. Version 9 adds `running_attempts`;
older audits gain `[]` without rewriting their existing values.
Errors roll back both writes; the command returns one JSON changed/refused
result and exits nonzero on refusal. No task, attempt, intent, reservation,
receipt, log, PR proof or selection snapshot is edited. A restart with the new
config accepts the persisted new limit; a config still containing the old limit
continues to fail the ordinary mismatch guard. `status` remains read-only and
reports the effective persisted limit even with a mismatched config. Historical
selection snapshots retain their original capacity and all existing
continuation/config-identity checks remain in force. Do not edit SQLite or switch
to a new state root as a substitute for this operation.

## Audited natural-exit continuation

`retry TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT [--revalidate-terminal-exit] --execute`
is separate from `resume`. It renders the current validated `resume` template
for exactly one new attempt after rechecking a naturally completed, released
latest attempt in `attention`. The original selection snapshot and task revision
stay unchanged. `retry_authorized` is private per-attempt evidence binding the
exact new launch plan to the old plan/config, new command templates, actor/reason,
fresh source/claim observation, absent PR lookup, and reservation. Gate and reconciliation readers accept a
new revision only through that exact authorization and unchanged task identity.
Successful `retry_pr_lookup` observations are appended to the new attempt in the
same transaction, never to the old attempt, and visible as redacted summaries
in `show`/`status`; launch arguments and configuration remain private.

Only command templates can change. Sources, mappings, principals, roots and
capacity must equal the stored snapshot. Natural receipts must have an exit code,
no signal, no sent stop signals, and no stop intent. Receipt files, launch/dispatch
records, child/supervisor identities, log lengths and current absence of registered
processes/groups/descendants must agree. The new launch intent, audit evidence and
reservation commit together under the coordinator lock. Failures before that
transaction create no attempt; launch uncertainty afterwards retains the slot.

Luthor does not validate worker flags or their values, including `--max-tool-calls`;
the worker reports its own CLI errors. Credential-bearing arguments and bad
templates remain rejected. Operator entry points print the fixed validation reason
for an invalid configuration, never serde input. Profile contents, executable availability and
provider behavior belong to the configured worker. Stored historical snapshots are not rewritten or rejected
merely because they contain flags the worker later rejects; the current private
configuration must pass validation. Both its templates must be valid.

Normal stop/resume invariants and stored-selection semantics are unchanged. A
changed-revision retry cannot subsequently use normal `resume` to adopt that
configuration. No startup, scheduler or reconciliation path launches a retry.


### Darwin boot identity and terminal startup revalidation

New Darwin process registrations use the validated, lowercase
`darwin-bootsessionuuid:<UUID>` identity from `kern.bootsessionuuid`. Empty,
malformed, nil, non-UTF-8 or unavailable UUID output fails closed. There is no
`kern.boottime` fallback. Process-start identity still comes from
`PROC_PIDTBSDINFO`; its PID and start timestamp checks are unchanged.

Ordinary retry requires the recorded child, supervisor and tracked descendants to agree
on the current boot identity. Independent PID and group probes must all return
ESRCH. A live or reused PID, present group, permission error, unavailable identity
or contradictory record refuses continuation before a new reservation or launch.
Retry holds expose fixed reconciliation messages; they do not print recorded
identities, paths, command arguments or external error details.

Historical `kern.boottime` records remain unchanged and cannot establish boot
continuity with a new UUID. The observed historical value
`{ sec = 1790533213, usec = 116017 } Sun Sep 27 15:20:13 2026` and later value
`{ sec = 1790533213, usec = 220969 } Sun Sep 27 15:20:13 2026` illustrate why
whole-string comparison was unstable. Equal seconds/date do not establish a
boot-session identity. Present-day ESRCH probes establish current absence of
registered IDs, but cannot recover the boot-session identity of an exited
historical process. An operator statement or an audit recording those same
observations supplies no independent continuity evidence.

Ordinary historical retry still refuses with:

```text
luthor: retry refused or held: historical Darwin boot identity cannot prove boot continuity
```

`--revalidate-terminal-exit` selects an additional, narrow terminal proof for an
already completed and released native CLI startup rejection. It does not claim
boot continuity. All ordinary receipt, registration, claim, worktree, source,
author, capacity and exhaustive PR checks remain required. The extra proof requires:

1. An exact, already reconciled supervisor receipt from `try_wait`/`waitpid`, with
   natural exit code 2, no signal or stop, and matching private drained logs.
2. A direct configured `llxprt-code-rs` executable, one saved
   `--max-tool-calls 1024` argument, and the entire stdout equal to the native
   CLI's single JSON startup error for that limit and task session. Stderr must
   be empty. Extra output, a different session, another error or another executable
   cannot supply this proof.
3. Internally consistent original child/supervisor/gate registrations, no recorded
   descendants, and current ESRCH for both registered PIDs and both dedicated
   process groups. Reused PIDs, surviving group members, zombies and permission
   errors refuse. No signal is sent during revalidation.
4. A readable current boot identity recorded only as a new observation. No old
   identity is replaced or compared by seconds, date or microseconds.

The native CLI validates this limit before constructing its session, backend,
profiling runtime or tools. Consequently this specific rejection never launched
tool subprocesses that could escape the registered groups. Log EOF alone would
not prove that fact: a runtime descendant can close its pipes and escape. Generic
historical natural exits, missing telemetry and any tracked-descendant evidence
therefore remain held. This proof relies on the configured, trusted native CLI's
startup contract. The executable name and diagnostic do not constitute executable
attestation; arbitrary or untrusted programs that imitate that contract are not
supported. For issue #2, the configured native binary and its early validation
path were inspected and reproduced independently with a synthetic session.

The new `retry_authorized` record retains the old receipt, exact registration/gate
payloads, full startup diagnostic, observation time, current boot observation,
and the `native_max_tool_calls_preflight` basis. It also binds the actor/reason,
old/new plans and configs, new revision/reservation, fresh source item/issue and
claim, verified worktree snapshot, and fresh exhaustive PR absence. The transaction
rechecks the old receipt and payloads, source identity, task phase and capacity.
The new supervisor accepts the new revision only through that audit. Historical
attempt rows, their evidence/intent payloads and files remain unchanged.

This path supports issue #2's original startup rejection without creating a
successor task, moving its source claim, replacing its worktree/branch history,
editing SQLite or launching a duplicate worker. The operator must invoke the
verified binary explicitly; no daemon or reconciliation path authorizes it.

## Verified PR repair

The execute daemon now rechecks retained verified PRs for base conflicts and
failed **required** checks. Failed Actions jobs get one audited rerun per head
before a code repair; passing reruns are recorded as flaky-looking. Pending
checks and unknown mergeability wait, optional failures do not trigger repair,
and unsupported/non-rerunnable required checks hold for a person.

A task gets at most three repair continuations on its original session/worktree.
Each prompt identifies the existing PR, failing checks and bounded log excerpts;
it requires merge-only base integration, unchanged gates and same-branch push.
After process accounting, the worker's uncommitted
`.luthor-pr-repair-result.json` must identify the new attempt and status
`repaired` or `semantic_conflict`. Semantic conflicts and invalid reports hold
for a person rather than completing or automatically retrying. New PR proofs
are appended; `show`/`status` display the latest verified attempt. See
[issue 44 validation](issue44-pr-repair-validation.md) for safety details.
A task whose issue closed or whose PR finished is retired instead of held; see
[issue 54](issue54-retire-finished.md).

## Opt-in verified-head merging (issue 40)

Each mapping may have a `merge` policy. Omission or `{"kind":"disabled"}`
leaves PR completion, repair and cleanup behavior unchanged; preview mode never
merges. No operator configuration is automatically rewritten or enabled.

```json
"merge": {
  "kind": "enabled",
  "method": "squash",
  "required_checks": {"kind": "all"},
  "allow_draft": false
}
```

`method` is required and is `merge`, `squash` or `rebase`. The independent merge
`required_checks` rule defaults to `all`; it also accepts `{"kind":"github"}`
(branch-protection-required contexts) or an explicit nonempty names array such
as `["Luthor quality gates"]`. A missing named check is pending, not success.
Selected checks must finish SUCCESS or SKIPPED (legacy commit statuses must be
SUCCESS). Neutral, failed, cancelled, timed-out and action-required checks do
not qualify. The default `all` includes optional failures; rules selecting
fewer contexts never bypass GitHub branch protection. Check observations retain
names, selected/optional status, conclusions, source and Actions job identities.
An empty rollup has no checks for `all`/`github`; explicit missing names still
block. GitHub must independently report the PR MERGEABLE and CLEAN.

Drafts default to excluded. `allow_draft: true` explicitly authorizes marking an
otherwise qualifying draft ready for review before merging; GitHub cannot merge
an actual draft. Eligibility is rechecked after promotion. There is no admin,
auto-merge or protection bypass, and no branch deletion in the merge operation.

The execute daemon sweeps `pr_complete` tasks before terminal cleanup, using the
current mapping's opt-in policy. The saved selection/worktree/attempt must be
accounted and all existing mapping fields must still match. Changing **only**
merge policy neither reauthorizes workers nor changes their ownership checks.
The exact saved PR ID, repository IDs, author, branches, tracker line and head
SHA must match fresh observations. A newer pushed head is never adopted by this
sweep. An independently verified repair/base-merge head can qualify on a later
cycle, using its newer saved proof.

Before a merge or draft promotion, a task-level `pr_merge` intent journals the verified PR,
policy, check summary and observation time. The gh REST merge write includes
`sha=<verified head>` and `merge_method=<method>`, so a racing push is refused by
GitHub. Authentication must match the saved, mapping-allowed author and the existing
`acoliver` live-write restriction before each write. Mutations
are single-shot and never use read retries. The response and fresh terminal PR
must agree on the merge commit; `pr_merged` records the authorization, merged
head, merge commit and confirmation time. The task remains in `pr_complete`
until the existing cleanup/retirement path settles it.

An interrupted/uncertain mutation records a bounded `pr_merge_error`, is not
blindly repeated, and holds only that task (see "Task isolation" below). The next
enabled sweep can recover only from exact terminal merged-PR proof on the
authorized head. An unresolved open PR requires operator inspection; it is not
reported as merged or sent to a repair worker. With policy disabled, the sweep performs no upstream reads or
writes, including recovery writes. Policy drift never allows a different head
or repository to satisfy an old intent.

### BEHIND branches and tracker closure

For an owned, non-excluded BEHIND PR whose selected checks are green, the merge
sweep records a `pr_branch_update` intent bound to the saved head, then uses
GitHub's `updatePullRequestBranch` through `gh api graphql` with
`expectedHeadOid=<observed head>` and explicit `updateMethod: MERGE`.
A successful response records `pr_branch_updated`. This is a merge of the base,
never a rebase, force push or protection bypass. Each head has its own durable
intent; an uncertain update is not blindly repeated.

The sweep never authorizes merging the returned/new head. It waits for the
existing repair monitor to verify fresh claim, accounted exit/receipt, worktree
ownership and configured-base merge lineage, synchronize a clean owned local
worktree, and record `pr_head_verified`. Only a subsequent sweep can evaluate
checks for that verified new head. Old-head checks cannot authorize it. A
conflicting PR goes to the existing bounded conflict repair; an update error is
recorded and surfaced, not hidden by a fallback.

After an exact terminal merge is confirmed, Luthor reads the tracker identity
from the saved selection (including repository, issue number, node ID and URL),
not from the code repository or closing keyword. If GitHub already closed it,
`tracker_closed` records that observation without a write. Otherwise a
`tracker_close` intent precedes a single `gh issue close --reason completed`
with a comment naming the merged PR, verified head and merge commit. Fresh
closure proof is journaled as `tracker_closed`, bound to `pr_merged`.
This works for non-default PR bases and separate tracker repositories.

An interrupted closure retains the merge evidence. A later sweep can recover
from a fresh exact closed-issue read without remerging/reclosing; an unresolved
open issue requires inspection rather than replaying an uncertain comment or
close. While a Luthor merge has no recorded closure, the task is held and
terminal cleanup retains its worktree and branch, so the sweep can finish it.
No tracker issue is closed before confirmed merge evidence exists. Disabled
policy performs neither branch updates nor tracker closure, even on restart.

### Task isolation and externally merged PRs (issue 78)

An error in the merge step for one task never fails the execute cycle. Upstream
read/write errors and verification refusals are journaled as `pr_merge_error`
(an identical latest error is not appended again), the task is reported under
`pr_merge.waiting` as `merge step failed: <reason>`, stays `pr_complete`, and the
remaining tasks and scheduling continue with exit 0. Only a failure of Luthor's
own state store (unreadable or inconsistent local records) still stops the cycle.

The identity checks that precede a Luthor merge, draft promotion, branch update
or tracker closure are unchanged: the saved verified head must still equal the
live head, and nothing is written otherwise.

When no Luthor merge intent exists and GitHub reports the saved PR `merged` at
a head different from the saved verified head, the sweep decides whether the
delivery is verifiable without writing to GitHub:

1. Every identity field except the head (PR id, number, URL, repositories,
   branches, author, creation time and the exact `Tracker-Issue` line) must
   match the saved proof, with the PR `closed`, a `merged_at` time and a merge
   commit.
2. The head must have advanced from the saved verified head only by first-parent
   merges of the configured base, using the same lineage proof as PR repair
   (`worktree::pr_head`, issue 69). The head comes from the PR's own
   `refs/pull/N/head`, because the head branch is commonly deleted on merge; the
   task worktree is read but never changed.

If both hold, the existing retirement path records the task as `pr_merged`. The
`task_retired` evidence carries the saved and observed heads, merge commit and
base-merge lineage, and `luthor_merge_evidence: "none"`: no `pr_merge`/`pr_merged`
authorization or merge proof is fabricated and the tracker is left to GitHub or
the operator as for any other external merge. The sweep reports it under
`pr_merge.delivered`. Any other difference (changed identity, a non-base-merge
commit, a merge parent not on the base, an unreadable lineage, a task whose
attempt has no verified exit) holds only that task with that reason. A PR merged
at the saved head is unchanged: it waits as "not open and unmerged" for cleanup.

Daemon JSON reports `pr_merge.merged`, `pr_merge.delivered` and `pr_merge.waiting`; failing check names
are included in waiting reasons even for UNSTABLE PRs. `status` and `show`
expose cached `last_merge_checks`, `merge_evidence`, `merge_error`,
`last_branch_update` and `tracker_closure`, as well as the existing
`last_pr_health`. These are journal observations, not fresh reads.
