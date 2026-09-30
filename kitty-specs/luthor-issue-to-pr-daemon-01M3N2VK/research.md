# Research: Luthor issue-to-PR daemon

## Scope and method

This research consolidates the existing design material for planning. It is based on source inspection of `dev-docs/architecture.md`, `dev-docs/notes.md`, `dev-docs/implementation-plan.md`, and the mission `spec.md` and `plan.md`. It does not include live GitHub/API probes, a daemon implementation, or runtime tests. Source-backed descriptions below must not be treated as live capability verification.

## Findings and decisions

### Issue identity and source eligibility

A source is a configured GitHub Project, repository scope, readiness marker/value, and optional exact milestone. Enumerate Project items and validate each issue with direct reads for open state, repository scope, readiness, assignees, and milestone; issue-list results alone do not establish Project membership. Use stable tracker repository and issue identities, distinct from the mapped code repository. These requirements are recorded in `dev-docs/architecture.md` and `kitty-specs/luthor-issue-to-pr-daemon-01M3N2VK/spec.md` (FR-001–FR-003).

The marker's actual representation, Project API access, pagination behavior, and field semantics remain unverified externally. Historical notes in `dev-docs/notes.md` explicitly caution that existing Luther labels and observed project metadata do not establish a Luthor source rule.

### Claiming is not an atomic lock

Persist claim intent, make one assignment write, then independently read issue and Project state. Require the expected principal as sole assignee and verify that assignment alone removes eligibility while preserving the readiness marker. Ambiguous writes or changed source state hold the task; do not retry automatically. GitHub assignment has no compare-and-set, so a successful read cannot guarantee exclusivity against other dispatchers or people. Live dispatch therefore depends on a maintainable single-dispatcher policy, or a separate atomic claim mechanism if that policy cannot be enforced. This is a design constraint, not a verified operational arrangement (`dev-docs/architecture.md`; `dev-docs/implementation-plan.md`, preflight and package 2).

### Separate task, attempt, and PR evidence

Keep task lifecycle distinct from attempt lifecycle and outcome. Persist task identity, source/mapping, configuration revision, intents, reservations, ordered evidence, attempts, and PR observations. A zero exit is not task completion. PR lookup has distinct `open`, `absent`, `ambiguous`, and `error` outcomes; absence requires exhaustive successful pagination. Completion requires an open PR matching exact tracker issue URL, mapped repository, base/head context, task branch, and configured authorization. Draft and check status are recorded but do not gate completion (`dev-docs/architecture.md`; `spec.md`, FR-006 and FR-008).

### Worktree, agent session, and recovery

Use a daemon-owned unique worktree and branch per task, preserving path and filesystem identity. Agent commands are configured as executable plus argument arrays, without shell interpolation. The design selects llxprt-code-rs as the first target and describes `--session ID --cwd WORKTREE -p PROMPT`; resume uses the same session/root/worktree with a distinct continuation prompt after the prior attempt is accounted for. Existing notes identify these as source-inspected behavior, not an installed-binary run. The command contract, session-root resolution, and signal handling require a no-write runtime dry run before reliance.

Persist side-effect intents and reserve capacity before launch. A detached supervisor owns the child process group, logs both streams, and writes a durable receipt. Recovery must verify process identity and termination before releasing a slot. Uncertain state is held; no automatic retry or redispatch follows. These are design decisions and acceptance requirements, not test results (`dev-docs/architecture.md`; `dev-docs/implementation-plan.md`, packages 2–5).

### Scope and delivery boundary

V1 stops at a verified open PR. It excludes automatic retries, review, repair, feedback loops, merge, and unassignment. The mission is documentation/specification only; this research performed no GitHub writes and no daemon implementation. Later acceptance calls for five distinct Luthor-produced open PRs for distinct `vybestack/llxprt-code` issues with milestone `0.12.0`, but this is a future acceptance target, not an achieved outcome (`dev-docs/implementation-plan.md`; `spec.md`, FR-010 and C-005).

## Open questions and external verification

1. Can the configured Project be read with complete item membership, pagination, required fields, and the chosen ready marker? What is the actual marker representation?
2. Does the authorized account have required Project/issue read and assignment rights, and can a single-dispatcher policy be maintained for every source?
3. What are the currently authorized account and the actual code-repository push/head permissions at acceptance time? No live identity or permission probe was run here.
4. Does the installed llxprt-code-rs binary support the described initial/resume arguments and stable session-root behavior, and how does it behave under SIGINT/SIGTERM?
5. How will the empty remote's first issue-branch PR workflow be bootstrapped without a direct-main push or fabricated base branch? This needs authorized operational confirmation before remote writes.
6. Acceptance issues and readiness markers must be selected/configured before live dispatch; no issue was selected or changed by this research.

Detailed failure-injection and acceptance checks are listed in `dev-docs/implementation-plan.md` and `kitty-specs/luthor-issue-to-pr-daemon-01M3N2VK/plan.md`; they are proposed validation work, not tests performed.
