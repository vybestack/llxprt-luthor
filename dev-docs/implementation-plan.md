# Luthor implementation plan

## Outcome and boundaries

Build a small Rust daemon and local control CLI that discover eligible GitHub Project issues, claim them by assignment, run a configured headless agent in daemon-owned worktrees, and stop when a verified open PR exists. The first agent target is `llxprt-code-rs`. The first delivery is a runnable implementation pushed to an issue branch in `vybestack/llxprt-luthor`, followed by supervised end-to-end acceptance: five distinct open PRs from Luthor tasks for five distinct `vybestack/llxprt-code` issues with milestone `0.12.0`.

V1 does not review, repair, merge, unassign, or retry automatically. Draft PRs and failing or pending checks count; report check status without gating completion. A successful agent exit is not completion. No manual issue code fixes or manually created PRs count toward acceptance. “By Luthor” means a traceable daemon task, claim, worktree, branch, attempt, and matching PR. It does not require a separate bot author identity. Use the currently authorized `acoliver` account; never use the suspended `llxprt` machine account. Do not merge without Andrew's explicit approval.

This workspace is not yet a Git repository. Do not initialize it, create a remote repository, issue, branch, label, or push, or implement code under this planning task. Validate repository access and bootstrap needs first. If the requested initial push is interpreted as a push to the default branch, stop at the runnable issue-branch PR; default-branch updates require explicit approval to merge.

## Ordered work packages

### 1. Repository and live-source preflight

Confirm the intended `vybestack/llxprt-luthor` repository exists or establish the authorized bootstrap path, SSH access, active GitHub identity, and push/PR permissions. Check branch protections and identify a non-main issue branch workflow. No remote or local repository mutations occur until implementation is authorized.

Validate Project API access before fixing the source adapter contract: Project and item enumeration, issue membership, pagination, field reads, and the configured ready marker as either a repository label or a Project field/value. Verify direct issue reads expose state, full assignee list, milestone, and stable IDs. Confirm the account can assign tracker issues and create PRs/push branches to the mapped code repository. Confirm the single-dispatcher operating policy can be maintained. GitHub assignment has no compare-and-set; a write plus read cannot guarantee exclusive claim against another actor. If competing dispatchers cannot be excluded, hold live dispatch until an atomic claim service or exclusive queue exists; do not describe the claim as race-free.

**Acceptance checks:** a documented access matrix and tested read-only probes establish the exact source representation, permissions, pagination behavior, mappings, and identity. Any missing or ambiguous prerequisite is a named blocker, not “no issues found.”

### 2. Planning package and executable contracts

Define the initial JSON configuration, adapter result contracts, executable argument arrays, state transitions, and SQLite migrations. Keep credentials out of configuration and logs. Support Project-based eligibility, mapped tracker-to-code repositories, ready marker, no assignee, optional exact milestone, global concurrency, agent initial/resume commands, and private state/worktree roots. Store a revision of nonsecret effective configuration with tasks and attempts. Specify exact binaries, arguments, JSON schema, and migrations in this package after preflight confirms available interfaces; do not add speculative adapter or agent frameworks.

Define stable task identity from tracker repository ID and issue node ID. Persist claim intent before one assignment write. Verify assignment and re-read issue plus Project item; require the configured assignee as sole assignee and the retained marker/membership/milestone. Ambiguity or changed source state holds the task, with no automatic write retry. Create a unique task branch and worktree only after claim verification. Persist task, attempt, event, reservation, intent, and ordered evidence.

**Acceptance checks:** schema and migration tests cover fresh install, upgrade, transaction rollback, duplicate task keys, and preservation of held reservations. Contract tests reject malformed/incomplete adapter results and distinguish absent, ambiguous, and failed reads.

### 3. Rust daemon, adapters, and controls

Implement one coordinator per state directory, protected by an interprocess lock and transactional SQLite slot reservations. Add structured GitHub adapters that enumerate Project items, validate eligibility using direct reads, assign once, and independently verify claim. Add PR lookup states `open`, `absent`, `ambiguous`, and `error`; only exhaustive successful pagination can establish absence. Validate exact `Tracker-Issue: https://github.com/OWNER/REPO/issues/NUMBER` linkage, target repository/base, configured head repository and unique task branch, PR ID, and authorized account. Record draft state and available checks as evidence, not gates.

Implement daemon-owned worktrees, task/attempt lifecycle, a detached supervisor mode, and local `status`, `show`, `logs`, `pause`, `resume`, and `reconcile` controls. Persist launch/stop intents before side effects. Supervisor drains both streams, writes durable receipts, and uses process start/boot identities and a handshake, never a bare PID, for recovery or signaling. Retain slot reservations until process-group termination is verified. On uncertain process, receipt, log, or PR evidence, hold the task; never redispatch it. Resume is explicit, uses the same rs session ID and worktree/root identity, a new attempt ID, and a distinct continuation prompt. Silence warns without killing. Keep per-attempt telemetry and conservative holds.

**Acceptance checks:** unit and integration tests prove transitions and authorization boundaries, one dispatcher per state directory, no oversubscription, no duplicate task launch, no secret logging, and no slot release without termination evidence. CLI output exposes reason, evidence timestamp, attempt, process/worktree identity, last output, PR URL, and checks.

### 4. Fake-GitHub and fake-agent crash suite

Before any real dispatch, test the entire coordinator/supervisor path with deterministic fake adapters and child processes. Inject crashes at claim intent/write/read, partial worktree creation, launch registration/gate release, active execution, stop escalation, log sync, receipt write/rename, and coordinator receipt ingestion. Exercise duplicate Project items, source changes, other assignees, ambiguous assignment, pre-existing/conflicting PRs, failed/incomplete pagination, exact issue-link mismatch, zero/nonzero/signaled exits, pause/resume, supervisor death, PID reuse, escaped descendants, disk/SQLite failures, and restart with uncertain reservations.

**Acceptance checks:** every uncertainty results in a hold or explicit attention state; no held task is launched again; no capacity is reused without verified termination; no agent exit substitutes for PR evidence; only a matching open PR completes. Draft and red-check PRs pass. Crash tests demonstrate recovery without duplicate launches or invented exit codes.

### 5. Real rs dry run

Run the configured `llxprt-code-rs` executable against a disposable local/test issue source with GitHub writes disabled. Verify executable and arguments, stable session root, `--session ID --cwd WORKTREE -p PROMPT`, worktree identity, stdout/stderr capture, stop behavior, process accounting, and resume with a distinct prompt after interruption. Confirm the installed binary's behavior, since architecture notes describe source inspection rather than an end-to-end run. Exercise daemon restart and reconciliation.

**Acceptance checks:** dry-run evidence shows no GitHub writes, correct isolated worktree/session, durable per-attempt telemetry and receipts, verified stop and resume, and no dispatch after uncertain recovery. Any mismatch returns to the relevant package; no live workaround or retry.

### 6. Runnable branch delivery and live acceptance

Once implementation is authorized, create an issue branch, implement and verify the runnable daemon, then push that branch to `vybestack/llxprt-luthor` and open its PR. Never push directly to main. Routine PR creation and reporting need no owner-approval gate; merge only with Andrew's explicit approval. Before live dispatch, recheck identity, permissions, source configuration, single-dispatcher policy, concurrency, and source/target mappings.

Andrew selects and marks eligible issues before acceptance. Use the configured Project and ready marker, require open/unassigned issues, and require milestone `0.12.0` when configured. Luthor assigns each issue to claim it and leaves the marker in place. Dispatch only through Luthor. Do not manually fix issue code or create acceptance PRs. Run conservatively, maintaining per-attempt telemetry and reconciling slots before every dispatch. No automatic retry: failed or uncertain tasks wait for explicit operator action. Stop once five distinct qualifying PRs are verified.

**Acceptance checks:** five unique tracker issue IDs and five unique open PR IDs, each for `vybestack/llxprt-code`, each issue with milestone `0.12.0`, and each PR with exact issue linkage, configured base/head context, authorized account, and recorded Luthor task/claim/attempt provenance. Draft status and red/pending checks do not fail acceptance. An existing, closed, merged, ambiguous, manually created, or pre-dispatch PR does not count. No review, merge, or green-check requirement is implied.

## Delivery evidence and blocker path

For each of the five tasks retain Project and item IDs, marker value, milestone ID/title, initial issue state and assignees, claim intent and direct verification reads, source/config revision, task and attempt IDs, worktree path and filesystem identity, branch/base/head, executable and prompt revision, session ID, process lifecycle and receipts, log paths/byte counts, PR ID/URL/author/repository/base/head/issue-link evidence, draft status, check summaries, timestamps, and operator actions. Keep secrets out of evidence. Preserve failures and warnings alongside later PR success.

If blocked, pause dispatch and record the failed prerequisite, exact evidence/API/OS error, affected tasks and reserved slots, and the smallest remediation needed. Fix configuration or adapter contracts in the appropriate package, rerun affected fake tests, then repeat the rs dry run before resuming live dispatch. For uncertain claims or processes, do not repeat writes or free slots; reconcile through fresh reads and verified process evidence. If Project access, claim exclusivity, credentials, or PR identity cannot be established, do not proceed to live acceptance. Spec Kitty mission scaffolding is deferred: this is a docs-first, uninitialized workspace and no mission was created.