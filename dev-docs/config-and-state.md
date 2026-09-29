# Configuration and state contract

Configuration is a JSON object with `state_root`, `worktree_root`, positive `capacity`, non-empty `sources` and `mappings`, and `initial`/`resume` command templates. Each source has a Project ID, repository names (`owner/repo`), one `ready_marker`, and optional exact `milestone`. A marker is either `{ "kind": "label", "name": "..." }` or `{ "kind": "project_field", "name": "...", "value": "..." }`. Each mapping identifies tracker and code repositories, checkout path and base branch. Commands contain an executable path and argv array. Supported literal substitutions are `{task.issue_number}`, `{task.repository}`, `{task.issue_url}`, `{task.id}`, `{attempt.id}` and `{worktree}`. These are argument templates, not shell: shell expansion/operators are rejected. Keep credentials in the external credential store/environment, never configuration or argv.

Synthetic example (all paths, IDs and names are illustrative; no key is present):

```json
{
  "state_root": "/private/luthor/state",
  "worktree_root": "/private/luthor/worktrees",
  "capacity": 1,
  "sources": [{"project_id":"PVT_SAMPLE","repositories":["example/tracker"],"ready_marker":{"kind":"label","name":"daemon-ready"},"milestone":"v1"}],
  "mappings": [{"tracker_repository":"example/tracker","code_repository":"example/code","checkout":"/src/example-code","base_branch":"main"}],
  "initial": {"executable":"/usr/local/bin/agent","args":["--prompt","Work on {task.issue_url}","--cwd","{worktree}"]},
  "resume": {"executable":"/usr/local/bin/agent","args":["--continue","{attempt.id}","{worktree}"]}
}
```

The marker name/value and optional milestone are exact selectors. Omitting milestone removes only that eligibility check. State uses SQLite schema version 2 with task, attempt, intent, reservation, evidence and metadata tables. Opening a fresh database creates the current schema; versions above 2 are rejected. No historical schema migration is implemented or claimed. Configured capacity is persisted and must match on later opens. A process-level coordinator lock (where used) supplements database transactions; reservations remain authoritative and uncertain work must retain capacity.
