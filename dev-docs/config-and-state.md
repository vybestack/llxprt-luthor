# Configuration and state contract

Configuration is a JSON object with `state_root`, `worktree_root`, positive `capacity`, non-empty `sources` and `mappings`, and `initial`/`resume` command templates. Each source has a Project ID, repository names (`owner/repo`), one `ready_marker`, and optional exact `milestone`. A marker is either `{ "kind": "label", "name": "..." }` or `{ "kind": "project_field", "name": "...", "value": "..." }`. Each mapping identifies tracker and code repositories, checkout path, base branch, push remote, allowed PR head repository, and allowed PR author. Commands contain an executable path and argv array. Supported literal substitutions are `{task.issue_number}`, `{task.repository}`, `{task.issue_url}`, `{task.id}`, `{attempt.id}` and `{worktree}`. These are argument templates, not shell: shell expansion/operators are rejected. Keep credentials in the external credential store/environment, never configuration or argv.

Synthetic example (all paths, IDs and names are illustrative; no key is present):

```json
{
  "state_root": "/private/luthor/state",
  "worktree_root": "/private/luthor/worktrees",
  "capacity": 1,
  "sources": [{"project_id":"PVT_SAMPLE","repositories":["example/tracker"],"ready_marker":{"kind":"label","name":"daemon-ready"},"milestone":"v1"}],
  "mappings": [{"tracker_repository":"example/tracker","code_repository":"example/code","checkout":"/src/example-code","base_branch":"main","push_remote":"git@github-acoliver:example/code.git","allowed_pr_head_repository":"example/code-fork","allowed_pr_author":"example-user"}],
  "initial": {"executable":"/usr/local/bin/agent","args":["--prompt","Work on {task.issue_url}","--cwd","{worktree}"]},
  "resume": {"executable":"/usr/local/bin/agent","args":["--continue","{attempt.id}","{worktree}"]}
}
```

The marker name/value and optional milestone are exact selectors. Omitting milestone removes only that eligibility check. State uses SQLite schema version 3 with task, attempt, intent, reservation, evidence and metadata tables. Reservations are durable per-attempt history keyed by attempt ID, with a partial unique index allowing at most one active reservation per task. Opening a fresh database creates the current schema. Version 1 databases gain persisted capacity metadata, and version 1 or 2 databases are upgraded transactionally to version 3, preserving task, attempt, intent, reservation and evidence rows. If a migration cannot complete, its schema changes and version update roll back together. Versions above 3 are rejected. Configured capacity is persisted and must match on later opens. Creating a task stores a typed selection-evidence record in the same transaction as task identity, preserving the selected Project and item, issue URL and identity, marker, milestone, repository mapping, observation time, source, configuration revision, and a typed effective configuration snapshot (state/worktree roots, capacity, sources, mappings, and initial/resume executable and argv). The snapshot is validated before task creation and committed atomically with the task and selection evidence. Credentials are excluded by configuration validation; invalid-configuration errors do not echo supplied values or raw JSON. Duplicate task creation rolls back without leaving a second selection record. A process-level coordinator lock (where used) supplements database transactions; reservations remain authoritative and uncertain work must retain capacity.
