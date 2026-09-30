# Data model findings

This is a research-level conceptual model derived from `dev-docs/architecture.md`, `dev-docs/implementation-plan.md`, and the mission `spec.md`. It describes required concepts; it is not a finalized database schema or migration.

## Entities and relationships

- **Source**: Configured Project identity, allowed tracker repositories, readiness marker/value, optional milestone, and source rules. A source yields Project items and their issue evidence. Actual API fields and marker representation remain externally unverified.
- **Repository mapping**: Associates a tracker repository with a code repository, checkout, base branch, push remote, allowed head repository/account, and worktree root. A tracker issue can map to a different code repository. Mapping ambiguity blocks claiming.
- **Task**: Durable unit keyed by stable tracker repository ID plus issue node ID. Stores operator-facing issue reference, selected source and source evidence, mapping/config revision, task state, intents, reservations, and ordered evidence. Retained even when no longer discoverable.
- **Attempt**: One configured agent execution belonging to a task. Has unique attempt ID, lifecycle and separate outcome, session/config/prompt identity, worktree identity, process/supervisor identity, log paths/counts, timestamps, and receipt evidence. Multiple attempts are possible only through explicit operator resume after accounting for the prior attempt.
- **Worktree/branch**: Daemon-owned execution isolation associated with a task and mapping. Stores intended and resolved path, filesystem identity, branch, and base. Must be unique and reconciled rather than adopting unknown local state.
- **Claim evidence**: Ordered intent, assignment operation, and subsequent direct issue/Project observations. Proves observed assignment conditions, not exclusive ownership against independent actors because GitHub assignment is not compare-and-set.
- **PR observation**: Timestamped lookup result `open | absent | ambiguous | error`, retaining repository/PR IDs, URL, author, base/head identities, exact tracker issue link, draft/check summaries, and lookup evidence. Only verified matching `open` can complete a task.
- **Reservation**: Capacity slot associated with a task/attempt and its launch/process evidence. Remains reserved across restart while a process may still be live or uncertain; release requires verified termination.
- **Event/evidence record**: Ordered, timestamped record tied to task and optionally attempt, identifying stage, source, result, and bounded error details. Supports reconciliation and operator-facing reasons.
- **Supervisor receipt**: Durable attempt artifact with attempt/process identities, exit or signal, timestamps, and stream byte counts. Must match persisted attempt and logs; missing or conflicting receipt requires reconciliation.

## State distinctions

Task states in the architecture are `preparing`, `running`, `pause_requested`, `paused`, `held`, `attention`, and `pr_complete`. Attempt lifecycle is independently `launch_intended`, `starting`, `running`, `stop_intended`, `ended`, or `reconciliation_required`; outcomes are `exit(code)`, `signal(number)`, `launch_failed(reason)`, or explicitly resolved telemetry loss with unavailable exit status. Do not infer task completion from attempt outcome.

## Identity and cardinality

- One source can contain many Project issue items; overlapping sources may refer to the same tracker issue and require deduplication and unambiguous source selection.
- One tracker issue maps to one task identity; a task has one selected mapping and may have sequential, explicitly authorized attempts.
- Each attempt uses one task worktree/session identity and can yield zero or more PR observations. At most one matching PR can satisfy the task's completion predicate; ambiguity blocks completion.
- Reservations correspond to capacity usage and cannot be cleared solely because a coordinator restarted or a receipt was written.

Exact SQL tables, constraints, enum encoding, migration strategy, and adapter payload schemas remain planning/implementation decisions. No runtime or external API facts were established in this research.
