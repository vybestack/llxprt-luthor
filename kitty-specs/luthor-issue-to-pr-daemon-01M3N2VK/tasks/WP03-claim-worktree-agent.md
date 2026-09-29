---
work_package_id: WP03
title: Verified claim, worktree and agent supervision
dependencies:
- WP02
requirement_refs:
- FR-003
- FR-004
- FR-005
- FR-006
- FR-007
- NFR-001
- NFR-002
- NFR-003
- NFR-004
planning_base_branch: work/luthor-issue-to-pr-daemon
merge_target_branch: work/luthor-issue-to-pr-daemon
branch_strategy: Planning artifacts for this mission were generated on work/luthor-issue-to-pr-daemon. During /spec-kitty.implement this WP may branch from a dependency-specific base, but completed changes must merge back into work/luthor-issue-to-pr-daemon unless the human explicitly redirects the landing branch.
subtasks:
- T006
- T007
- T008
phase: Phase 2 - Execution
history:
- timestamp: '2026-09-28T22:43:00Z'
  agent: system
  action: Authored from implementation plan and architecture.
authoritative_surface: src/
create_intent:
- src/claim.rs
- src/worktree.rs
- src/github/pull_request.rs
- src/supervisor.rs
- src/coordinator.rs
- src/cli.rs
- tests/claim.rs
- tests/worktree.rs
- tests/supervisor.rs
- tests/coordinator.rs
- tests/cli.rs
execution_mode: code_change
owned_files:
- src/main.rs
- src/lib.rs
- src/claim.rs
- src/state.rs
- src/github/pull_request.rs
- src/worktree.rs
- src/supervisor.rs
- src/coordinator.rs
- src/cli.rs
- tests/claim.rs
- tests/worktree.rs
- tests/supervisor.rs
- tests/coordinator.rs
- tests/cli.rs
tags: []
tracker_refs: []
---

# Work Package Prompt: WP03 - Verified claim, worktree and agent supervision

## Goal
Claim eligible work safely, isolate changes per task and account for configured agent processes across normal execution and interruption.

## Requirements
**Requirement Refs**: FR-003, FR-004, FR-005, FR-006, FR-007, NFR-001, NFR-002, NFR-003, NFR-004

## Scope
- Persist claim intent, perform one assignment write, and independently verify expected sole assignee, unchanged source readiness and exclusion due to assignment. Ambiguity holds; never retry automatically.
- Implement reusable, exhaustive open-PR lookup for pre-claim, pre-launch and pre-resume checks. A successfully paginated absent result permits progress; matching pre-existing PRs, ambiguous matches, incomplete pagination and errors block claim or launch. Do not count a pre-existing PR as this task's submission.
- Check mappings and pre-existing PRs before claim; create unique daemon-owned task branch/worktree only after verified claim; reconcile partial work without adopting unknown state.
- Implement coordinator scheduling and detached same-binary supervisor with gated child start, process identity, private stdout/stderr and durable receipt.
- Persist intents before launch/stop; preserve reservations until process group termination is verified; reconcile before dispatch after restart.
- Implement local `status`, `show`, `logs`, `pause`, `resume`, and `reconcile` controls.
- Pause one task, retaining claim/worktree/session; resume only by explicit request with same session/root/worktree, new attempt and distinct continuation prompt.
- Support configured llxprt-code-rs initial/resume commands without hardcoding its CLI into scheduling logic.

## Out of Scope
Automatic retries, unassignment, agent code repair, reviews, merges, daemon-specific agent drivers, and treating missing telemetry as an exit result.

## Verification
Fake GitHub tests must distinguish exhaustive absent, matching pre-existing PR, ambiguous, incomplete and failed reads before claim, launch and resume. Fake child tests cover launch gates, exit/signal receipts, log failures, pause escalation, verified capacity release, task-level continuation and other-task progress. Inject coordinator/supervisor termination before launch, during execution, around stop and receipt ingestion. Assert no held-task relaunch, no PID-only signal, no uncertain slot reuse and no two active attempts for one task on macOS and Linux.

## Completion Evidence
Focused integration and crash tests plus local CLI examples showing state, reason, process evidence, worktree/session identity and durable logs.
