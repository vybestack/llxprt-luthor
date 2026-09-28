---
work_package_id: WP01
title: Preflight and adapter contracts
dependencies: []
requirement_refs:
- C-002
- C-004
- FR-001
- FR-002
- FR-003
- FR-005
- FR-008
- FR-011
- NFR-005
planning_base_branch: work/luthor-issue-to-pr-daemon
merge_target_branch: work/luthor-issue-to-pr-daemon
branch_strategy: Planning artifacts for this mission were generated on work/luthor-issue-to-pr-daemon. During /spec-kitty.implement this WP may branch from a dependency-specific base, but completed changes must merge back into work/luthor-issue-to-pr-daemon unless the human explicitly redirects the landing branch.
subtasks:
- T001
- T002
phase: Phase 0 - Preflight
history:
- timestamp: '2026-09-28T22:43:00Z'
  agent: system
  action: Authored from implementation plan and architecture.
authoritative_surface: dev-docs/
create_intent: []
execution_mode: code_change
scope: codebase-wide
owned_files:
- dev-docs/**
- src/**
- tests/**
tags: []
tracker_refs: []
---

# Work Package Prompt: WP01 - Preflight and adapter contracts

## Goal
Resolve external contracts and create implementation-ready interface decisions before daemon code depends on them.

## Requirements
**Requirement Refs**: FR-001, FR-002, FR-003, FR-005, FR-008, FR-011, NFR-005, C-002, C-004

## Scope
- Verify read-only GitHub Project item enumeration, membership, pagination, fields and selected ready-marker representation.
- Confirm direct issue reads expose stable IDs, state, all assignees and exact milestone; confirm PR lookup capabilities and permissions.
- Record authorized account, assignment/push/PR permissions, tracker-to-code mappings and operational single-dispatcher constraint.
- Validate empty-remote issue-branch PR bootstrap options without remote writes, fake default branches or direct-main pushes.
- Define typed adapter results and errors that distinguish absent, ambiguous, incomplete and failed data.
- Verify installed rs executable and no-write initial/resume command, session root, worktree identity and stop behavior.

## Out of Scope
No live assignment, issue/label creation, push, PR creation, merge or Rust daemon implementation.

## Verification
Produce an access/identity matrix and adapter contract notes. Include read-only probe evidence and exact unresolved blockers. Do not convert missing access or incomplete pagination into empty results. Mission implementation must not begin live dispatch if single-dispatcher policy or source contract is unresolved.

## Completion Evidence
Checked-in contract and preflight documentation, plus tests for normalized result/error parsing if adapter types are introduced in this WP.
