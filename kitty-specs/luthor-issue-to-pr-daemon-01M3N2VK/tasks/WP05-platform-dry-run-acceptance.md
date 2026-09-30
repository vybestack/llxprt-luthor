---
work_package_id: WP05
title: Cross-platform dry run and supervised acceptance
dependencies:
- WP01
- WP04
requirement_refs:
- C-003
- C-004
- C-005
- FR-010
- FR-011
- FR-012
- NFR-005
planning_base_branch: work/luthor-issue-to-pr-daemon
merge_target_branch: work/luthor-issue-to-pr-daemon
branch_strategy: Planning artifacts for this mission were generated on work/luthor-issue-to-pr-daemon. During /spec-kitty.implement this WP may branch from a dependency-specific base, but completed changes must merge back into work/luthor-issue-to-pr-daemon unless the human explicitly redirects the landing branch.
subtasks:
- T011
- T012
- T013
phase: Phase 4 - Delivery and acceptance
history:
- timestamp: '2026-09-28T22:43:00Z'
  agent: system
  action: Authored from implementation plan and architecture.
- timestamp: '2026-09-30T00:00:00Z'
  agent: system
  action: Recorded actual WP05 implementation ownership, including necessary shared-file edits previously approved under WP03/WP04, and operator documentation for runnable delivery; requirements and status are unchanged.
authoritative_surface: src/
create_intent:
- src/platform.rs
- tests/platform_dry_run.rs
- tests/acceptance_evidence.rs
- dev-docs/wp05-validation.md
- README.md
execution_mode: code_change
owned_files:
- src/daemon.rs
- src/coordinator.rs
- src/state.rs
- tests/state.rs
- tests/daemon.rs
- src/platform.rs
- src/lib.rs
- src/supervisor.rs
- tests/platform_dry_run.rs
- tests/supervisor.rs
- tests/acceptance_evidence.rs
- dev-docs/wp05-validation.md
- README.md
tags: []
tracker_refs: []
---

# Work Package Prompt: WP05 - Cross-platform dry run and supervised acceptance

## Goal
Verify the runnable daemon on macOS and Linux, then perform a separately authorized five-PR acceptance run through Luthor.

## Requirements
**Requirement Refs**: FR-010, FR-011, FR-012, NFR-005, C-003, C-004, C-005

## Scope
- Build and run automated checks on macOS and Linux; investigate OS-specific process and identity behavior.
- On macOS, run configured llxprt-code-rs against a disposable source with GitHub writes disabled; prove executable arguments, stable session/worktree/root, logs, stop, receipt and distinct-prompt resume.
- Resolve first-repository issue-branch PR bootstrap, prepare runnable branch delivery and PR, never direct-main push. Merge is outside this WP absent explicit approval.
- Document operator setup and use for runnable delivery in `README.md`.
- After implementation delivery and authorization, use five different ready-marked, open, unassigned `vybestack/llxprt-code` issues with milestone `0.12.0`, selected/labeled before dispatch.
- Run dispatch only through Luthor, no manual code or PR fixes; collect task/claim/worktree/attempt/authorized identity and exact PR linkage evidence for five distinct open PRs.

## Out of Scope
The analysis/setup gate creates planning artifacts only. This later WP covers implementation verification and delivery, but does not authorize pushes, issue creation, labels, merges or live acceptance before their prerequisites and authorization are satisfied.

## Verification
No-write rs dry run must prove the installed binary behavior. Live acceptance must verify five unique issue and open PR identities, milestone, exact tracker reference, configured target/base/head/account, and current draft/check status. Draft and non-green checks count. If source access, identity, permissions, single-dispatcher policy, or empty-remote bootstrap is unresolved, stop and record the exact blocker.

## Completion Evidence
Cross-platform build/test results, no-write dry-run record, issue-branch PR reference, and (only after separately authorized live acceptance) the five-item evidence table. No acceptance PR may be attributed to a manual fix.
