---
work_package_id: "WP05"
title: "Cross-platform dry run and supervised acceptance"
dependencies: ["WP01", "WP04"]
planning_base_branch: "work/luthor-issue-to-pr-daemon"
merge_target_branch: "work/luthor-issue-to-pr-daemon"
branch_strategy: "Planning artifacts were generated on work/luthor-issue-to-pr-daemon; completed changes must merge back into work/luthor-issue-to-pr-daemon."
subtasks:
  - "T001"
  - "T002"
  - "T003"
phase: "Phase 4 - Delivery and acceptance"
assignee: ""
agent: ""
shell_pid: ""
history:
  - timestamp: "2026-09-28T22:43:00Z"
    agent: "system"
    action: "Authored from implementation plan and architecture."
---

# Work Package Prompt: WP05 - Cross-platform dry run and supervised acceptance

## Goal
Verify the runnable daemon on macOS and Linux, then perform a separately authorized five-PR acceptance run through Luthor.

## Requirements
Requirement References: FR-005, FR-008, FR-009, FR-010, FR-011, C-003, C-004

## Scope
- Build and run automated checks on macOS and Linux; investigate OS-specific process and identity behavior.
- On macOS, run configured llxprt-code-rs against a disposable source with GitHub writes disabled; prove executable arguments, stable session/worktree/root, logs, stop, receipt and distinct-prompt resume.
- Resolve first-repository issue-branch PR bootstrap, prepare runnable branch delivery and PR, never direct-main push. Merge is outside this WP absent explicit approval.
- After implementation delivery and authorization, use five different ready-marked, open, unassigned `vybestack/llxprt-code` issues with milestone `0.12.0`, selected/labeled before dispatch.
- Run dispatch only through Luthor, no manual code or PR fixes; collect task/claim/worktree/attempt/authorized identity and exact PR linkage evidence for five distinct open PRs.

## Out of Scope
This mission setup's local changes are planning artifacts only. This WP does not authorize implementation beyond its later mission scope, pushes, issue creation, labels, merges, or live acceptance before prerequisites and authorization are satisfied.

## Verification
No-write rs dry run must prove the installed binary behavior. Live acceptance must verify five unique issue and open PR identities, milestone, exact tracker reference, configured target/base/head/account, and current draft/check status. Draft and non-green checks count. If source access, identity, permissions, single-dispatcher policy, or empty-remote bootstrap is unresolved, stop and record the exact blocker.

## Completion Evidence
Cross-platform build/test results, no-write dry-run record, issue-branch PR reference, and (only after separately authorized live acceptance) the five-item evidence table. No acceptance PR may be attributed to a manual fix.
