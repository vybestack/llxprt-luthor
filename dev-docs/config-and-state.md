# Configuration and state contract

Configuration is a JSON object with `state_root`, `worktree_root`, positive `capacity`, an explicit non-secret `assignment_login` (the configured issue assignee, independent of the allowed PR author), non-empty `sources` and `mappings`, and `initial`/`resume` command templates. Each source has a Project ID, repository names (`owner/repo`), one `ready_marker`, and optional exact `milestone`. A marker is either `{ "kind": "label", "name": "..." }` or `{ "kind": "project_field", "name": "...", "value": "..." }`. Each mapping identifies tracker and code repositories, checkout path, base branch, push remote, allowed PR head repository, and allowed PR author. The allowed PR head repository may be either the in-repository tracker or code repository (including when they are the same repository) or an authorized fork; it must use `owner/repo` format. Commands contain an executable path and argv array. Supported literal substitutions are `{task.issue_number}`, `{task.repository}`, `{task.issue_url}`, `{task.id}`, `{attempt.id}` and `{worktree}`. These are argument templates, not shell: shell expansion/operators are rejected. Worker argv accepts only llxprt-code-rs flags verified from its headless help: value flags `--session`, `--turn`, `--branch`, `--profile`, `--profile-load`, `--cwd`, `-p`/`--prompt`, `--mem-profile`, `--max-tool-calls`, `--turn-time`, `--max-shell-output`, `--max-tool-output`, `--max-turn-output`, `--digest-size-floor`, `--model-params-mode`, `--request-timeout`; switches `--allow-insecure-http`, `--allow-shell`, `--print-config`, `-h`/`--help`, `-V`/`--version`. Header/env injection flags and unknown options are rejected. A value-taking option consumes one following argument or an `--option=value` argument. Keep credentials in the external credential store/environment, never configuration or argv.

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

The marker name/value and optional milestone are exact selectors. Omitting milestone removes only that eligibility check. State uses SQLite schema version 3 with task, attempt, intent, reservation, evidence and metadata tables. Reservations are durable per-attempt history keyed by attempt ID, with a partial unique index allowing at most one active reservation per task. Opening a fresh database creates the current schema. Version 1 databases gain persisted capacity metadata, and version 1 or 2 databases are upgraded transactionally to version 3, preserving task, attempt, intent, reservation and evidence rows. If a migration cannot complete, its schema changes and version update roll back together. Versions above 3 are rejected. Configured capacity is persisted and must match on later opens. Creating a task stores a typed selection-evidence record in the same transaction as task identity, preserving the selected Project and item, issue URL and identity, marker, milestone, repository mapping, observation time, source, configuration revision, and a typed effective configuration snapshot (state/worktree roots, capacity, sources, mappings, and initial/resume executable and argv). The snapshot is validated before task creation and committed atomically with the task and selection evidence. Credentials are excluded by configuration validation; invalid-configuration errors do not echo supplied values or raw JSON. Duplicate task creation rolls back without leaving a second selection record. A process-level coordinator lock (where used) supplements database transactions; reservations remain authoritative and uncertain work must retain capacity.


## Audited natural-exit continuation

`retry TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT --execute`
is separate from `resume`. It renders the current validated `resume` template
for exactly one new attempt after rechecking a naturally completed, released
latest attempt in `attention`. The original selection snapshot and task revision
stay unchanged. `retry_authorized` is private per-attempt evidence binding the
exact new launch plan to the old plan/config, new command templates, actor/reason,
fresh absent PR lookup, and reservation. Gate and reconciliation readers accept a
new revision only through that exact authorization and unchanged task identity.
`retry_pr_lookup` observations are append-only and visible as redacted summaries
in `show`/`status`; launch arguments and configuration remain private.

Only command templates can change. Sources, mappings, principals, roots and
capacity must equal the stored snapshot. Natural receipts must have an exit code,
no signal, no sent stop signals, and no stop intent. Receipt files, launch/dispatch
records, child/supervisor identities, log lengths and current absence of registered
processes/groups/descendants must agree. The new launch intent, audit evidence and
reservation commit together under the coordinator lock. Failures before that
transaction create no attempt; launch uncertainty afterwards retains the slot.

Luthor validates the observed native `--max-tool-calls` boundary: exactly one
literal value, `-1` or `1..512`, in either separated or inline form. Unknown flags,
credential-bearing arguments, bad templates and shell operators remain rejected.
Other native flag value semantics, profile contents, executable availability and
provider behavior belong to the configured worker. Revalidate its help when
changing worker builds. Stored historical snapshots are not rewritten or rejected
merely because they contain the old unsupported budget; the current private
configuration must pass validation. Both its templates must be valid.

Normal stop/resume invariants and stored-selection semantics are unchanged. A
changed-revision retry cannot subsequently use normal `resume` to adopt that
configuration. No startup, scheduler or reconciliation path launches a retry.
