# Mission Specification: Luthor issue-to-PR daemon

**Mission Branch**: `work/luthor-issue-to-pr-daemon`  
**Created**: 2026-09-28  
**Status**: Draft  
**Input**: Existing brief in `dev-docs/implementation-plan.md`, grounded in `dev-docs/architecture.md` and `dev-docs/notes.md`.

## User Scenarios & Testing

### User Story 1 - Dispatch eligible work safely (Priority: P1)

An operator configures issue sources, readiness rules, tracker-to-code repository mappings, a concurrency limit, and a headless agent command. Luthor finds eligible Project issues, verifies eligibility from Project membership and direct issue reads, claims by assigning the configured principal, confirms assignment and that assignment alone makes the issue ineligible for discovery, then launches the configured agent in its own task worktree.

**Why this priority**: Reliable, auditable dispatch is the primary purpose and prevents duplicate or unintended work.

**Independent Test**: Fake GitHub and fake agent exercise selection through process launch; assert one claim, verified exclusion, a unique worktree and no duplicate launch after restart.

**Acceptance Scenarios**:
1. Given an open Project issue with the configured ready marker, no assignees and matching optional milestone, when discovery runs, then Luthor records source evidence and can claim it once.
2. Given assignment succeeds, when direct issue and Project reads are repeated, then the expected principal is the sole assignee, the marker remains, and the issue fails eligibility because it is assigned.
3. Given a claim write or verification is ambiguous, when dispatch evaluates the task, then it holds the task and does not retry the write or launch an agent.
4. Given a ready issue was selected and labeled before dispatch, when Luthor runs, then it uses configured Project/marker/milestone rules and does not treat an existing Luther label or an unassigned issue alone as readiness.

### User Story 2 - Supervise, inspect, pause and resume (Priority: P1)

An operator sees task state, attempt outcome, logs, liveness, reason evidence, reserved capacity and PR status. The operator can pause one task without stopping others and explicitly resume it after the prior attempt is accounted for.

**Why this priority**: The daemon must make unattended process execution recoverable and controllable.

**Independent Test**: Deterministic child-process tests cover logs, stop, restart reconciliation and same-session continuation without GitHub writes.

**Acceptance Scenarios**:
1. Given a configured concurrency limit, when tasks launch, then no more than that many verified live or reserved attempts run, including across coordinator restart.
2. Given a task is paused, when its process group is confirmed stopped, then its slot is freed while claim, worktree, logs and session identity remain; other tasks can proceed.
3. Given an accounted paused task with fresh absent-PR evidence, when the operator resumes it, then Luthor uses the same rs session and worktree with a new attempt ID and a distinct continuation prompt.
4. Given process, receipt or PR evidence is uncertain, when restart reconciliation runs, then the task is held and no new turn starts; capacity is retained unless termination is proven.
5. Given an agent is silent, when status is queried, then Luthor warns without killing it.

### User Story 3 - Complete only on a verified PR and meet acceptance (Priority: P1)

The operator can trust that task completion means a matching open PR exists in the mapped code repository. After implementation and authorization, the system can be run to produce five distinct qualifying PRs for milestone `0.12.0` issues.

**Why this priority**: A process exit or agent statement cannot demonstrate that the requested deliverable exists.

**Independent Test**: Fake PR adapter tests verify all match conditions; supervised acceptance records evidence for five distinct Luthor tasks and PRs.

**Acceptance Scenarios**:
1. Given an exhaustive PR lookup finds a matching open PR with exact tracker issue linkage, expected repository/base/head and authorized identity, when evidence is refreshed, then the task completes even if the PR is draft or checks are failing or pending, with check status reported.
2. Given an agent exits successfully but no matching open PR exists, when the attempt is accounted for, then the task requires operator attention and is not complete.
3. Given five different open, unassigned, ready-marked `vybestack/llxprt-code` issues with milestone `0.12.0` are selected and labeled before dispatch, when acceptance runs through Luthor only, then five distinct matching open PR IDs are recorded with task, claim, worktree, attempt and linkage evidence; no manual code or PR fixes count.

## Edge Cases

- Project pagination, membership, readiness, milestone, assignment, PR lookup, or credentials are missing, stale, inconsistent or unavailable.
- Duplicate issue items or overlapping sources have conflicting markers, mappings or configuration.
- Another actor assigns or changes an issue during claim; GitHub assignment is not compare-and-set.
- Worktree creation is partial, branch identity conflicts, or tracker and code repositories differ.
- Agent exits nonzero, is signaled, loses logs, exceeds stop escalation, or leaves a process group that cannot be accounted for.
- Coordinator or supervisor stops at any persisted intent, gated launch, log sync, receipt, or ingestion boundary; process IDs can be reused.
- PR lookup is absent, ambiguous, incomplete, unauthorized, closed, merged, unrelated, or has unexpected head/base/linkage.
- Resume cannot preserve the same session root, path identity, worktree or distinct-prompt contract.
- State or log storage fails; secrets or issue-provided text appear in diagnostics.

## Requirements

### Functional Requirements

| ID | Title | User Story | Priority | Status |
|----|-------|------------|----------|--------|
| FR-001 | Configure sources | As an operator, I want JSON-configured GitHub Project sources, allowed repositories, readiness marker/value, optional exact milestone, mappings, concurrency, state/worktree roots and agent commands so that selection and execution follow explicit rules. | High | Open |
| FR-002 | Discover eligible issues | As an operator, I want Project items enumerated and checked against direct issue reads for open state, configured marker, no assignees, and optional milestone so that only eligible issues enter dispatch. | High | Open |
| FR-003 | Claim before dispatch | As an operator, I want claim intent persisted, assignment performed once, and independent reads to verify sole expected assignee and exclusion due to assignment while retaining readiness marker so that ambiguous claims are held. | High | Open |
| FR-004 | Isolate task execution | As an operator, I want a unique daemon-owned worktree and branch for each tracker issue, separate from Jefe, so agent changes cannot collide with other tasks. | High | Open |
| FR-005 | Run configured agents | As an operator, I want executable and argument templates for initial and continuation runs so the daemon can start with llxprt-code-rs without hardcoding an agent CLI. | High | Open |
| FR-006 | Supervise and limit work | As an operator, I want durable task/attempt state, logs, liveness, reasons, receipts, and global capacity reservations so restart cannot duplicate work or oversubscribe agents. | High | Open |
| FR-007 | Pause and resume tasks | As an operator, I want per-task pause and explicit resume with preserved claim/worktree/session, verified stop, and a distinct continuation prompt so other work proceeds safely. | High | Open |
| FR-008 | Verify PR completion | As an operator, I want exhaustive PR lookup to distinguish open, absent, ambiguous and error and validate exact issue link, repository, base/head and authorized identity so only a matching open PR completes a task. | High | Open |
| FR-009 | Provide local controls | As an operator, I want local `status`, `show`, `logs`, `pause`, `resume` and `reconcile` controls that expose current evidence and reasons. | High | Open |
| FR-010 | Accept supervised outcomes | As a maintainer, I want five distinct open PRs from Luthor attempts for five distinct `vybestack/llxprt-code` issues at milestone `0.12.0`, selected and labeled before dispatch, with no manual code or PR fixes counted. | High | Open |
| FR-011 | Support local platforms | As an operator, I want the CLI and daemon to run on macOS and Linux, initially tested on macOS. | High | Open |
| FR-012 | Keep v1 bounded | As a maintainer, I want no automatic retry, review, repair, feedback loop, merge or unassignment, and no completion based on agent exit or check status. | High | Open |

### Non-Functional Requirements

| ID | Title | Requirement | Category | Priority | Status |
|----|-------|-------------|----------|----------|--------|
| NFR-001 | Single state-directory coordination | At most one coordinator may dispatch for a state directory; reservation transactions enforce the configured limit across restarts and concurrent local CLI requests. | Reliability | High | Open |
| NFR-002 | Evidence before side effects | Every claim, worktree, launch and stop action has a durable intent first; a failed durable write prevents the next side effect. | Reliability | High | Open |
| NFR-003 | Conservative recovery | No held task is relaunched and no slot is released without verified process-group termination; missing or conflicting evidence remains visible. | Reliability | High | Open |
| NFR-004 | Secret handling | Credentials are excluded from configuration, prompts, events and diagnostics; local state, receipts and logs use private permissions. | Security | High | Open |
| NFR-005 | Platform support | Automated build/test checks cover macOS and Linux for supported CLI and process-supervision behavior. | Compatibility | High | Open |

### Constraints

| ID | Title | Constraint | Category | Priority | Status |
|----|-------|------------|----------|----------|--------|
| C-001 | Agent owns code work | Luthor selects, claims, supervises and verifies; configured agents perform software changes and PR creation. | Scope | High | Open |
| C-002 | No unsafe claim guarantee | GitHub assignment is not compare-and-set; live dispatch requires a maintainable single-dispatcher policy or a future atomic claim service. | Operational | High | Open |
| C-003 | No direct-main delivery | Initial runnable delivery must be pushed on an issue branch and proposed via PR; never push directly to main. | Git | High | Open |
| C-004 | Empty remote bootstrap | The remote currently has no default branch. Establish the first remote history through the approved issue-branch PR workflow; do not invent a default branch or push directly to main. | Git | High | Open |
| C-005 | No implementation in setup mission work | This task produces mission artifacts only; Rust daemon implementation and live issue/PR activity are out of scope. | Scope | High | Open |

### Key Entities

- **Source**: Project identity, repository scope, ready marker, optional milestone and eligibility evidence.
- **Task**: Stable tracker repository and issue identity, selected source, mapping, state and evidence.
- **Attempt**: A configured agent invocation with session, worktree, process, logs, receipt and outcome.
- **PR evidence**: Verified open/absent/ambiguous/error result, exact tracker link, repository/head identity, authorization and check summary.

## Success Criteria

### Measurable Outcomes

- **SC-001**: Fake-adapter tests show only Project member issues meeting every configured selector are eligible; ambiguous claims cause zero agent launches.
- **SC-002**: Across injected coordinator/supervisor failures, no task receives duplicate active attempts and no uncertain process slot is reused.
- **SC-003**: An accounted zero exit without matching PR never completes a task; only a matching verified open PR does.
- **SC-004**: Local control CLI runs on macOS and Linux and can inspect task state, evidence and logs and request pause, resume and reconciliation.
- **SC-005**: Supervised acceptance records five unique qualifying issue IDs and five unique open PR IDs with milestone `0.12.0`, Luthor provenance, exact linkage and configured identity/head evidence.

## Open Validation Decisions

- Confirm Project API membership, pagination, field access and ready-marker representation before locking the live adapter contract.
- Confirm the single-dispatcher policy and account permissions before live writes; do not describe assignment as race-free.
- Confirm installed rs headless initial/resume arguments, session-root identity and signal behavior in a no-write dry run.
- Confirm empty-remote bootstrap mechanics for an issue-branch PR without setting a fake default branch or pushing main; defer remote writes until separately authorized.
