---
work_package_id: WP04
title: PR proof and recovery validation
dependencies:
- WP03
requirement_refs:
- FR-006
- FR-008
- FR-009
- FR-012
- NFR-002
- NFR-003
- NFR-004
- NFR-005
planning_base_branch: work/luthor-issue-to-pr-daemon
merge_target_branch: work/luthor-issue-to-pr-daemon
branch_strategy: Planning artifacts for this mission were generated on work/luthor-issue-to-pr-daemon. During /spec-kitty.implement this WP may branch from a dependency-specific base, but completed changes must merge back into work/luthor-issue-to-pr-daemon unless the human explicitly redirects the landing branch.
subtasks:
- T009
- T010
phase: Phase 3 - Verification
history:
- timestamp: '2026-09-28T22:43:00Z'
  agent: system
  action: Authored from implementation plan and architecture.
authoritative_surface: src/
create_intent:
- src/pr_evidence.rs
- src/recovery.rs
- tests/pr_evidence.rs
- tests/recovery.rs
- tests/fault_injection.rs
- src/cli.rs
- tests/cli.rs
- tests/state.rs
- tests/claim.rs
- tests/coordinator.rs
- tests/supervisor.rs
- src/main.rs
execution_mode: code_change
owned_files:
- src/github/pull_request.rs
- tests/state.rs
- tests/claim.rs
- tests/coordinator.rs
- tests/supervisor.rs
- src/coordinator.rs
- src/state.rs
- src/pr_evidence.rs
- src/recovery.rs
- tests/pr_evidence.rs
- tests/recovery.rs
- tests/fault_injection.rs
- src/cli.rs
- src/main.rs
- tests/cli.rs
tags: []
tracker_refs: []
---

# Work Package Prompt: WP04 - PR proof and recovery validation

## Goal
Make verified PR evidence the only completion path and demonstrate conservative behavior under failures and restarts.

## Requirements
**Requirement Refs**: FR-006, FR-008, FR-009, FR-012, NFR-002, NFR-003, NFR-004, NFR-005

## Scope
- Extend WP03's exhaustive pre-claim/pre-resume PR lookup for completion; preserve its open, absent, ambiguous and error outcomes and successful-pagination requirement for absence.
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

## Implementation note
The audited recovery implementation lives in `src/supervisor.rs`, `src/coordinator.rs`, `src/state.rs`, and `src/main.rs`. Real OS and SQLite rollback tests reside in `tests/supervisor.rs`. The optional planned paths `src/recovery.rs`, `tests/recovery.rs`, and `tests/fault_injection.rs` are organizational only; their absence does not remove the requirements above.
