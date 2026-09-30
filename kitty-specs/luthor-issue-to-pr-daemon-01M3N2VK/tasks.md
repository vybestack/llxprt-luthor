# Work Packages: Luthor issue-to-PR daemon

**Inputs**: `kitty-specs/luthor-issue-to-pr-daemon-01M3N2VK/spec.md` and `kitty-specs/luthor-issue-to-pr-daemon-01M3N2VK/plan.md`.
**Delivery**: Work on issue branches and merge into `work/luthor-issue-to-pr-daemon`; no direct-main push or live dispatch before the stated gates.

## Work Package WP01: Preflight and adapter contracts (Priority: P0)

**Goal**: Resolve external contracts and first-PR delivery path before daemon implementation.
**Independent Test**: Read-only probes and no-write rs dry run document access, pagination, command and identity behavior.
**Prompt**: `tasks/WP01-preflight-contracts.md`
**Requirement Refs**: FR-001, FR-002, FR-003, FR-005, FR-008, FR-011, NFR-005, C-002, C-004

### Included Subtasks
T001 Verify Project/issue/PR adapters and authorized identity with read-only probes.
T002 Document assignment, first-PR bootstrap, single-dispatcher rule and installed rs command/session contract without writes.

### Dependencies
None.

## Work Package WP02: Configuration, state and eligibility (Priority: P0)

**Goal**: Persist task state and identify fully eligible Project issues without writes.
**Independent Test**: Contract, SQLite and coordinator tests prove selection and reservation invariants.
**Prompt**: `tasks/WP02-state-and-eligibility.md`
**Requirement Refs**: FR-001, FR-002, FR-006, FR-009, FR-012, NFR-001, NFR-002, NFR-004

### Included Subtasks
T003 Add validated JSON configuration and typed adapter results with focused tests.
T004 Add SQLite migrations, task/attempt/intent/reservation records and transaction tests.
T005 Enumerate and verify Project eligibility, deduplication and coordinator capacity with fake-adapter tests.

### Dependencies
Depends on WP01.

## Work Package WP03: Verified claim, worktree and agent supervision (Priority: P1)

**Goal**: Make one verified claim per task after exhaustive pre-existing PR lookup, then run a configured agent in an isolated worktree with durable process evidence.
**Independent Test**: Fake GitHub and child-process tests prove PR absence and failure distinctions before claim/resume, claim verification, safe worktree creation, pause/resume and uncertain-capacity holds.
**Prompt**: `tasks/WP03-claim-worktree-agent.md`
**Requirement Refs**: FR-003, FR-004, FR-005, FR-006, FR-007, NFR-001, NFR-002, NFR-003, NFR-004

### Included Subtasks
T006 Implement reusable exhaustive open-PR lookup before claim/resume; persist assignment intent, assign once and independently verify claim and worktree ownership.
T007 Implement supervisor, capacity holds, receipts and restart reconciliation with crash tests.
T008 Implement local controls and explicit same-session resume with tests.

### Dependencies
Depends on WP02.

## Work Package WP04: PR proof and recovery validation (Priority: P1)

**Goal**: Extend WP03's lookup with exact open-PR completion evidence and retain uncertainty through failures.
**Independent Test**: Fault injection distinguishes absent, ambiguous and failed PR lookups and preserves reservations on uncertain process state.
**Prompt**: `tasks/WP04-pr-proof-recovery-tests.md`
**Requirement Refs**: FR-006, FR-008, FR-009, FR-012, NFR-002, NFR-003, NFR-004, NFR-005

### Included Subtasks
T009 Extend the exhaustive PR lookup with exact completion linkage checks and attempt/task outcome separation.
T010 Test crash, telemetry-loss, process identity and storage failure boundaries.

### Dependencies
Depends on WP03.

## Work Package WP05: Cross-platform dry run and supervised acceptance (Priority: P2)

**Goal**: Verify macOS/Linux delivery, then run separately authorized five-PR acceptance through Luthor.
**Independent Test**: Cross-platform tests, no-write rs dry run and (after authorization) five distinct verified open PR records.
**Prompt**: `tasks/WP05-platform-dry-run-acceptance.md`
**Requirement Refs**: FR-010, FR-011, FR-012, NFR-005, C-003, C-004, C-005

### Included Subtasks
T011 Build and test on macOS and Linux and record no-write installed-rs dry-run evidence.
T012 Resolve empty-remote PR bootstrap and prepare runnable issue-branch delivery without merging.
T013 After separate authorization and issue selection, dispatch through Luthor and record five distinct verified open PRs.

### Dependencies
Depends on WP01 and WP04.
