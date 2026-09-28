---
work_package_id: WP02
title: Configuration, state and eligibility
dependencies:
- WP01
requirement_refs:
- FR-001
- FR-002
- FR-006
- FR-009
- FR-012
- NFR-001
- NFR-002
- NFR-004
planning_base_branch: work/luthor-issue-to-pr-daemon
merge_target_branch: work/luthor-issue-to-pr-daemon
branch_strategy: Planning artifacts for this mission were generated on work/luthor-issue-to-pr-daemon. During /spec-kitty.implement this WP may branch from a dependency-specific base, but completed changes must merge back into work/luthor-issue-to-pr-daemon unless the human explicitly redirects the landing branch.
subtasks:
- T001
- T002
- T003
phase: Phase 1 - Foundation
history:
- timestamp: '2026-09-28T22:43:00Z'
  agent: system
  action: Authored from implementation plan and architecture.
authoritative_surface: src/
create_intent:
- Cargo.toml
- src/config.rs
- src/state.rs
- src/github/project.rs
- src/eligibility.rs
- tests/config.rs
- tests/state.rs
- tests/eligibility.rs
execution_mode: code_change
owned_files:
- Cargo.toml
- src/config.rs
- src/state.rs
- src/github/project.rs
- src/eligibility.rs
- tests/config.rs
- tests/state.rs
- tests/eligibility.rs
tags: []
tracker_refs: []
---

# Work Package Prompt: WP02 - Configuration, state and eligibility

## Goal
Implement the validated configuration, durable task model and read-only eligible-issue selection.

## Requirements
**Requirement Refs**: FR-001, FR-002, FR-006, FR-009, FR-012, NFR-001, NFR-002, NFR-004

## Scope
- Add JSON configuration for Project sources, repository scope, ready marker/value, optional milestone, mappings, roots, capacity and executable/argv templates.
- Validate templates and allowed task-value expansion without shell interpolation; keep credentials out of config and persisted diagnostics.
- Add SQLite migrations and transaction-backed task, attempt, intent, reservation and ordered evidence records with stable tracker issue identity.
- Enumerate Project items, validate directly read issue state/marker/assignees/milestone and deduplicate overlapping sources.
- Represent read failures, stale/inconsistent data and conflicting source/mapping as explicit errors or held candidates, never absence.
- Add interprocess coordinator lock and capacity reservation invariants for one state directory.

## Out of Scope
Assignment writes, worktrees, agent launch and PR completion.

## Verification
Unit/contract tests for invalid config, secrets exclusion, optional exact milestone, marker forms, Project membership, pagination failures, duplicate items, stale issues, SQLite fresh/migration/rollback and task-key uniqueness. Concurrent coordinator tests prove one dispatcher per state directory and no oversubscription.

## Completion Evidence
Migration and configuration documentation, deterministic adapter fakes, and passing focused tests proving only fully eligible Project issues become candidates.
