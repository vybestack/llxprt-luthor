---
work_package_id: "WP04"
title: "PR proof and recovery validation"
dependencies: ["WP03"]
planning_base_branch: "work/luthor-issue-to-pr-daemon"
merge_target_branch: "work/luthor-issue-to-pr-daemon"
branch_strategy: "Planning artifacts were generated on work/luthor-issue-to-pr-daemon; completed changes must merge back into work/luthor-issue-to-pr-daemon."
subtasks:
  - "T001"
  - "T002"
phase: "Phase 3 - Verification"
assignee: ""
agent: ""
shell_pid: ""
history:
  - timestamp: "2026-09-28T22:43:00Z"
    agent: "system"
    action: "Authored from implementation plan and architecture."
---

# Work Package Prompt: WP04 - PR proof and recovery validation

## Goal
Make verified PR evidence the only completion path and demonstrate conservative behavior under failures and restarts.

## Requirements
Requirement References: FR-006, FR-008, FR-009, FR-012, NFR-002, NFR-003, NFR-004

## Scope
- Implement PR lookup outcomes open, absent, ambiguous and error; absent requires exhaustive successful pagination.
- Verify exact tracker issue URL, target repository, base, head repository and unique task branch, PR identity and configured authorized account.
- Record draft state and checks as advisory evidence; a matching open draft or red-check PR counts.
- Keep attempt outcome distinct from task completion; exit zero without matching PR becomes attention, not completion.
- Add crash/fault suite for duplicate items, source changes, claim ambiguity, partial worktrees, PR conflicts, process identity reuse/escape, logs/storage errors, lost receipts and uncertain restart reservations.
- Implement audited telemetry-loss resolution only with verified process absence, inspection and fresh PR evidence; never fabricate exit status.

## Out of Scope
Automatic retries, review/repair loops, issue unassignment, merge or gating on CI/check status.

## Verification
Fake GitHub and child-process tests cover absent versus error versus ambiguity, exact link and account/head mismatch, open/draft/red checks, zero/nonzero/signaled exits, stop and receipt boundaries, PID reuse, escaped processes and storage failure. Assert uncertain evidence holds and capacity is not reused without termination proof.

## Completion Evidence
Passing failure-injection suite and event/log examples that retain original attempt outcomes alongside later PR evidence.
