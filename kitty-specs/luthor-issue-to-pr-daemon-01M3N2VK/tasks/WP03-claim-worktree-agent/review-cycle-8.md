---
affected_files: []
cycle_number: 8
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-29T15:44:46Z'
reviewer_agent: user
wp_id: WP03
---

# WP03 review feedback

## Blocking finding: published agent-command example cannot dispatch or resume

`dev-docs/config-and-state.md:15-16` supplies an initial command with no `--session` and a resume command using `{attempt.id}` as the session. The actual production path in `src/coordinator.rs:438-454` calls `src/supervisor.rs:317-375`, which requires `--session {task.id}` and returns `Conflict` before launching for the documented initial example. `src/supervisor.rs:375-455` requires the same task session on resume, so the published continuation example also fails. The continuation prompt has no `{attempt.id}`, which prevents a second continuation from being distinct from its predecessor. This contradicts the WP03 configured-agent and repeatable same-session resume contract and leaves the WP's requested local CLI completion examples absent.

Update the user-facing example to an executable initial/resume template using `--session {task.id}`, `--cwd {worktree}` and distinct `-p` prompts (include `{attempt.id}` in the continuation). Add local `status`, `show`, `logs`, `pause`, `reconcile` and `resume` examples demonstrating task/attempt, reason, worktree/session identity and durable logs. Assert the documented command template through the production `prepare_initial` and `prepare_resume` paths, rather than testing JSON parsing only. No live GitHub writes are needed.

WP04 depends on WP03 and shares its lane; coordinate its start after the corrected WP03 is available.
