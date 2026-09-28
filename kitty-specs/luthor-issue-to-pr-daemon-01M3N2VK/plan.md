# Implementation Plan: Luthor issue-to-PR daemon

## Mission Context

Implement a small Rust daemon and local CLI that selects configured GitHub Project issues, assigns them, runs a configured headless agent in an isolated daemon-owned worktree, and completes only after verifying a matching open PR. The detailed starting brief and evidence are in `dev-docs/implementation-plan.md`, `dev-docs/architecture.md`, and `dev-docs/notes.md`. Preserve the scope and constraints in `spec.md`.

## Technical Context

- **Language/runtime**: Rust, macOS and Linux.
- **Persistence**: SQLite task, attempt, intent, reservation and ordered evidence records; private per-attempt stdout/stderr and durable supervisor receipts.
- **Configuration**: JSON, executable paths plus argument arrays, no shell interpolation; credentials outside config and logs.
- **GitHub access**: Structured Project, issue, assignment and PR adapters. Project item enumeration and direct issue reads are required; issue-list filtering alone cannot establish Project membership.
- **Execution**: One coordinator per state directory, detached supervisor per agent attempt, daemon-owned worktrees, local control CLI. First configured agent is llxprt-code-rs; its arguments/session behavior require installed-binary dry-run validation.
- **Existing design**: `dev-docs/architecture.md` defines selection, state, claim, PR proof, process identity and restart rules. Treat unresolved adapter, concurrency and OS supervision details as validation work, not assumed capabilities.

## Architecture and Decisions

1. **Discovery and claim**: Enumerate configured Project items, validate eligibility by direct reads, persist intent, assign once, and verify sole configured assignee plus exclusion solely due to assignment. Preserve marker. GitHub assignment is not atomic: enforce one authorized dispatcher per source or hold live dispatch.
2. **Task ownership**: Key by stable tracker repository and issue identity. Map tracker repo separately from code repo. Persist source/config revision and evidence. Use unique task branch and worktree; do not adopt unknown paths or branches.
3. **Persistence and coordination**: Transactional SQLite and local interprocess locking coordinate one state directory. Persist intent before side effects. Reserve capacity for live or unverified groups across restarts.
4. **Supervision**: Same executable offers detached supervisor mode, gated child start, process/boot identity, durable stream logs and atomic synced exit receipt. Never signal a bare PID or free capacity without verified termination. Uncertain state is held and visible.
5. **Controls**: Local `status`, `show`, `logs`, `pause`, `resume`, and `reconcile`. Pause is per task, keeps claim and worktree, releases slot after verified stop. Resume is explicit, same session/root/worktree, new attempt and distinct prompt. No automatic retry.
6. **PR proof**: Exhaustive successful lookup distinguishes open, absent, ambiguous and error. Exact tracker URL, mapped target, base/head identities, task branch and authorized PR identity must match. Draft and check results are informational. Agent exit is never completion.
7. **Delivery boundary**: Initial runnable code delivery is an issue branch and PR. The remote is empty, so first-branch PR bootstrap must be resolved without creating a fake default branch or pushing main. No remote writes in this mission-setup task. The five-PR milestone acceptance is separate supervised acceptance after implementation and issue selection/labeling.

## Implementation Sequence

### Stage 0: Preflight and repository delivery path

Confirm SSH remote and current authorized identity, push/PR permissions, code-repo mappings, branch workflow, and empty-remote PR bootstrap options. Validate Project API item membership, pagination, fields/marker form, assignment permission, and single-dispatcher policy. Identify exact rs executable contract and installed session-root/signal behavior. Do not create live issues, labels, claims or PRs in this stage.

**Exit evidence**: documented access/identity matrix; read-only Project/issue/PR probes; tested local rs dry-run contract. Unresolved requirements are named blockers, not inferred support.

### Stage 1: Contracts and durable state

Define validated JSON config and command interpolation allowlist; normalized adapter result/error types; task and attempt state transitions; SQLite schema/migrations; event and evidence formats. Keep stable task and attempt identity, separate task state from attempt lifecycle/outcome, and document Unix process identity differences for macOS/Linux.

**Exit evidence**: schema and contract tests cover fresh database, migration, rollback, duplicate identity, malformed adapter payloads, and reservation retention.

### Stage 2: Read-only discovery, claim and worktree

Implement source enumeration and direct validation, deduplication/conflict handling, candidate reporting, mapping validation, a reusable exhaustive open-PR lookup for pre-claim and pre-resume absence checks, persisted single assignment intent, and independent claim verification. An absent result requires successful full pagination; ambiguous or failed reads block claim and resume. Add safe unique branch/worktree creation and reconciliation of partial state. No agent execution until claim, PR and worktree checks pass.

**Exit evidence**: fake adapter tests cover membership, pagination, eligibility, optional milestone, duplicate source, stale issue, ambiguous claim and no retry; worktree tests prove unknown paths/branches are not adopted or deleted.

### Stage 3: Coordinator, agent supervisor and local control

Implement capacity and task scheduling, durable launch intent, gated detached supervisor, process group management, stream logging, receipts, restart reconciliation, status/show/logs, pause, resume and reconcile. Implement only the verified configured-command contract; use rs as the initial configuration, not a daemon-specific hardcoded driver.

**Exit evidence**: fake child integration/crash tests show no duplicate active task, no uncertain capacity reuse, successful pause of one task while others run, and explicit same-session/distinct-prompt resume.

### Stage 4: PR verification and failure-injection suite

Extend the Stage 2 PR lookup with exact completion linkage/identity checks; do not defer the pre-claim or pre-resume lookup until this stage. Add tests across claim/worktree/launch/stop/log/receipt/PR boundaries, including daemon and supervisor crashes, PID reuse, escaped descendants, partial writes, API errors and telemetry-loss resolution. Verify absent differs from ambiguous or failed.

**Exit evidence**: only matching open PR completes; draft/red/pending checks pass as completion evidence; all uncertain cases hold or require explicit attention; no automatic retry, review, repair, merge or unassignment exists.

### Stage 5: macOS/Linux dry run and runnable branch delivery

Build/test both supported platforms. On macOS, run the configured rs binary against a disposable source with GitHub writes disabled; prove worktree/session/root identity, logs, stop, receipt and continuation behavior. Resolve empty-remote branch PR bootstrap, then prepare a runnable issue-branch delivery and PR. Do not push directly to main.

**Exit evidence**: reproducible build and test outputs, dry-run evidence with no live writes, and issue-branch PR ready for review. Merge remains subject to explicit authorization.

### Stage 6: Supervised live acceptance

After runnable delivery and required authorization, Andrew selects and labels five distinct eligible `vybestack/llxprt-code` issues with milestone `0.12.0` before Luthor dispatch. Revalidate source, identity, permissions and single-dispatcher policy. Dispatch only through Luthor, retain audit evidence, and stop at five distinct verified open PRs. Do not manually fix code or create counted PRs.

**Exit evidence**: five unique issue identities and PR IDs, exact tracker linkage, configured target/base/head/account, task/claim/attempt evidence and current open/draft/check status. No review, merge or green checks implied.

## Validation Strategy

Run contract/unit tests for config, adapter parsing, eligibility, state transitions and PR matching. Run SQLite transaction and cross-process coordinator tests. Run fake GitHub plus fake child integration tests with fault injection at each persisted intent and process/receipt boundary. Exercise restart reconciliation, slot invariants, pause and explicit resume, malformed/unavailable Project and PR results, OS identity behavior on macOS/Linux, and log/storage failures. Keep external acceptance distinct from local tests. Do not claim Project API support, exclusive assignment, or rs runtime continuation without direct verification.

## Risks and Open Questions

- Project API enumeration, field access, marker representation and pagination need live read-only validation.
- GitHub assignment cannot guarantee an exclusive claim across independent dispatchers; the single-dispatcher rule must be operationally maintained.
- Empty remote bootstrap for a first issue-branch PR needs confirmation; do not fabricate a base branch or bypass merge policy.
- Portable process/group identity and escaped child handling differ across macOS/Linux; uncertain descendants must hold capacity.
- Rs source inspection is not installed-binary testing; validate initial/resume arguments and session root before promising it.
- Acceptance issues and labels are intentionally not created by this mission-setup task; they must be selected and labeled before live dispatch.
