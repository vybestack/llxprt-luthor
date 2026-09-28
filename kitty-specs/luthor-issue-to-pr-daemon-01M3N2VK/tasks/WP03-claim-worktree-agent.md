---
work_package_id: "WP03"
title: "Verified claim, worktree and agent supervision"
dependencies: ["WP02"]
planning_base_branch: "work/luthor-issue-to-pr-daemon"
merge_target_branch: "work/luthor-issue-to-pr-daemon"
branch_strategy: "Planning artifacts were generated on work/luthor-issue-to-pr-daemon; completed changes must merge back into work/luthor-issue-to-pr-daemon."
subtasks:
  - "T001"
  - "T002"
  - "T003"
phase: "Phase 2 - Execution"
assignee: ""
agent: ""
shell_pid: ""
history:
  - timestamp: "2026-09-28T22:43:00Z"
    agent: "system"
    action: "Authored from implementation plan and architecture."
---

# Work Package Prompt: WP03 - Verified claim, worktree and agent supervision

## Goal
Claim eligible work safely, isolate changes per task and account for configured agent processes across normal execution and interruption.

## Requirements
Requirement References: FR-003, FR-004, FR-005, FR-006, FR-007, NFR-001, NFR-002, NFR-003

## Scope
- Persist claim intent, perform one assignment write, and independently verify expected sole assignee, unchanged source readiness and exclusion due to assignment. Ambiguity holds; never retry automatically.
- Check mappings and pre-existing PRs before claim; create unique daemon-owned task branch/worktree only after verified claim; reconcile partial work without adopting unknown state.
- Implement coordinator scheduling and detached same-binary supervisor with gated child start, process identity, private stdout/stderr and durable receipt.
- Persist intents before launch/stop; preserve reservations until process group termination is verified; reconcile before dispatch after restart.
- Implement local `status`, `show`, `logs`, `pause`, `resume`, and `reconcile` controls.
- Pause one task, retaining claim/worktree/session; resume only by explicit request with same session/root/worktree, new attempt and distinct continuation prompt.
- Support configured llxprt-code-rs initial/resume commands without hardcoding its CLI into scheduling logic.

## Out of Scope
Automatic retries, unassignment, agent code repair, reviews, merges, daemon-specific agent drivers, and treating missing telemetry as an exit result.

## Verification
Fake child tests for launch gates, exit/signal receipts, log failures, pause escalation, verified capacity release, task-level continuation and other-task progress. Inject coordinator/supervisor termination before launch, during execution, around stop and receipt ingestion. Assert no held-task relaunch, no PID-only signal, no uncertain slot reuse and no two active attempts for one task on macOS and Linux.

## Completion Evidence
Focused integration and crash tests plus local CLI examples showing state, reason, process evidence, worktree/session identity and durable logs.
