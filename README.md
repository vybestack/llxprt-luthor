# Luthor

Luthor is a local Rust daemon for selecting eligible GitHub Project issues, assigning them to a configured login, launching `llxprt-code-rs` in isolated worktrees, and checking whether each task produced a matching pull request. Discovery and operator inspection are read-only with respect to GitHub. Assignment, worker launch, pause, and reconciliation are separate actions with different effects; read the command behavior below before using them.

WP05 delivery is complete, and Luthor completed a separately authorized five-issue live acceptance run. The implementation delivery PR is [Luthor PR #1](https://github.com/vybestack/llxprt-luthor/pull/1); the five issue PRs and their observed check states are recorded in [WP05 validation](dev-docs/wp05-validation.md). Synthetic macOS/Linux dry-run results remain separate from that live evidence. Several issue PR checks are failing or still running, so this acceptance record does not claim green CI.

## Build and test offline

Install a Rust toolchain that supports the Rust 2024 edition. The dependencies are locked in `Cargo.lock`.

```sh
cargo build --offline --locked
cargo test --offline --locked --all-targets -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets -- -D warnings
```

The documented checks require dependencies to already be present in Cargo's local cache. `--offline` prevents Cargo from fetching missing packages. The test command runs the full target suite serially, as used in the recorded macOS and Linux validation. Linux format and Clippy results were not recorded because those tools were unavailable in the Linux image.

The binary is `target/debug/luthor` after a debug build. Use `cargo run --offline --locked -- --help` for the top-level help, or run the built binary directly:

```sh
target/debug/luthor --help
target/debug/luthor daemon --help
```

The actual top-level usage is:

```text
Usage: luthor discover --config <path>
       luthor daemon --config PATH --config-revision REV [--repository owner/repo --issues N,N,...] [--once] [--execute]
       luthor dispatch --config <path> --repository owner/repo --issue N --config-revision REV [--execute]
       luthor resume TASK --config <path> --execute
       luthor retry TASK --attempt ID --config PATH --config-revision REV --actor LOGIN --reason TEXT [--revalidate-terminal-exit] --execute
       luthor recover TASK --attempt ID --config PATH --actor LOGIN --reason TEXT --execute
       luthor status --config <path>
       luthor show TASK --config <path>
       luthor logs TASK [--attempt ATTEMPT] --config <path>
```

`pause` and `reconcile` are also implemented operator commands; their exact syntax is shown under [Inspect and control tasks](#inspect-and-control-tasks). `recover` is a narrow audited recovery operation for missing receipts, not a routine retry control.

## Configuration and access setup

Luthor requires a JSON configuration. The structs in `src/config.rs` reject unknown fields. The required top-level fields are:

| Field | Meaning |
| --- | --- |
| `state_root` | Private local directory for SQLite state and attempt artifacts. |
| `worktree_root` | Root directory for daemon-managed task worktrees. |
| `capacity` | Positive maximum number of reserved task attempts. |
| `assignment_login` | GitHub login Luthor assigns to selected issues. |
| `sources` | Non-empty set of Project sources and eligibility selectors. |
| `mappings` | Non-empty tracker-to-code repository and push/PR identity mappings. |
| `initial` | Executable and argv template for a new worker session. |
| `resume` | Executable and argv template for continuing that session. |

A source includes `project_id`, `repositories`, a `ready_marker`, and optional `milestone`. Markers are either `{"kind":"label","name":"…"}` or `{"kind":"project_field","name":"…","value":"…"}`. A mapping includes `tracker_repository`, `code_repository`, `checkout`, `base_branch`, `push_remote`, `allowed_pr_head_repository`, and `allowed_pr_author`. Command templates use an executable and an `args` array. They are argv templates, not shell commands. Supported substitutions are `{task.issue_number}`, `{task.repository}`, `{task.issue_url}`, `{task.id}`, `{attempt.id}`, and `{worktree}`.

Do not copy a configuration example into a live environment without validating every repository, Project, account, path, branch, and remote. The configuration contract and a synthetic example are maintained in [Configuration and state](dev-docs/config-and-state.md); validation rules and command-template constraints are enforced by `src/config.rs` and tested in `tests/config.rs`. Keep credentials out of JSON and argv. Luthor uses the installed `gh` executable for GitHub operations and requires its authenticated identity to match the configured allowed identity for the selected task. Do not assume an alternate credential or authentication fallback.

Before any live scheduling, verify all of the following independently:

- The Project ID, allowed tracker repositories, exact readiness marker, and optional exact milestone select only intended issues. Discovery requires Project membership, an open issue, the configured marker, no assignees, and a matching milestone when configured.
- `assignment_login` is the intended assignee and the authenticated GitHub identity is permitted for that task. GitHub assignment is not an atomic compare-and-set operation; Luthor verifies the assignment afterward and holds work when it cannot verify the result.
- Each tracker repository has exactly one valid mapping. Confirm the checkout and base branch, push remote, allowed PR head repository, and authorized PR author against the intended GitHub repositories and account.
- The configured SSH push remote works for the intended identity and repository. Configure SSH outside Luthor; never put credentials in the configuration or command arguments.
- `state_root` and `worktree_root` are local paths with appropriate access controls. Use a dedicated state directory and do not share it between independent Luthor installations.

## Preview and explicit execution

Start with read-only discovery:

```sh
target/debug/luthor discover --config /private/luthor/config.json
```

This calls GitHub Project and issue reads and prints eligible candidates as JSON lines. It does not assign or launch a worker.

Daemon mode without `--execute` performs preview cycles. Supply both `--repository` and `--issues` to restrict the selection to specific configured issue numbers; omit both to scan configured sources. `--once` exits after one cycle. Without it, preview repeats every 30 seconds.

```sh
target/debug/luthor daemon --config /private/luthor/config.json --config-revision local --once
# Optional target restriction, still preview-only:
target/debug/luthor daemon --config /private/luthor/config.json --config-revision local --repository owner/tracker --issues 17,18 --once
```

Only `--execute` authorizes daemon scheduling, including assignment and worker launch. Dispatch also requires `--execute`. These are GitHub and local-state side effects, not dry-run switches:

```sh
target/debug/luthor daemon --config /private/luthor/config.json --config-revision local --repository owner/tracker --issues 17 --once --execute
target/debug/luthor dispatch --config /private/luthor/config.json --repository owner/tracker --issue 17 --config-revision local --execute
```

The examples show syntax only. They are not a recommendation to run against live repositories. Live use requires a separate operational decision and a verified, safe preflight. Luthor claims by assigning the configured login, verifies the claim, prepares a task worktree, and launches the worker. An uncertain assignment or launch is held rather than automatically retried.

## Private state, capacity, and recovery

`state_root` contains `state.sqlite3` and private per-attempt artifacts, including worker logs and process evidence. The state implementation uses restrictive permissions for private artifacts. Keep the root on a trusted local filesystem and back it up only with appropriate protections. A process-level coordinator lock prevents simultaneous coordinators from dispatching against the same state root; database transactions and durable reservations also enforce capacity across restarts. Each task also has a private worktree-owner lock in the state root. Its exclusive file descriptor (FD) is passed from the coordinator through the supervisor to the worker, including across `exec`, so the reservation remains owned while that task's worker is alive even if the coordinator exits. The lock is per task, so separate tasks do not block one another. Luthor records the task ID, attempt ID, and lock file device/inode as evidence and checks that the open descriptor still names the expected file before relying on it.

If the owner lock cannot be acquired or verified, launch is refused and recorded as a bounded launch failure such as `launch_failed`; inspect the task and attempt evidence with `status` and `show`. Older attempts without owner-lock proof do not gain proof retroactively. Recovery of those attempts remains held when ownership or process quiescence cannot be established. Do not replace, remove, or recreate a lock file to clear a refusal: that changes its inode and can let two processes hold locks on different files under the same path. Do not release a task reservation just because the lock path appears free. Require the recorded attempt and process evidence to establish that the worker is accounted for.

This lock coordinates cooperative Luthor workers that follow the protocol. It does not contain arbitrary uncooperative programs running as the same user, which can ignore the lock or interfere with same-user files and processes. Use separate operating-system isolation for that threat boundary. Do not remove lock, reservation, or attempt artifacts to free capacity manually.

Capacity counts reserved attempts, including uncertain attempts. A slot is not released merely because a process appears absent or a worker exited. Luthor retains reservations when process, receipt, claim, or PR evidence is uncertain. Pause can free a slot only after process-group termination is verified; the task's claim, worktree, logs, and session identity remain. A paused task requires explicit resume. Each resume keeps the task's worker session and worktree and creates a new attempt.

A worker process exit does not complete a task. Completion requires a verified matching open PR in the mapped code repository, with the exact tracker issue linkage, expected base and allowed head repository, and authorized identity. Draft status and pending or failing checks are reported, but do not by themselves negate a verified matching open PR. An absent, ambiguous, or unreadable PR result does not establish completion. Luthor does not automatically retry work, review or repair a PR, merge, or unassign an issue.

## Inspect and control tasks

The read-only inspection commands open the local state database in read-only mode. They do not perform fresh GitHub reads. `status` reports task phases, reserved versus configured capacity, output-silence warnings, and stored evidence. `show` displays a task's attempts, worktree, events, and stored evidence. `logs` reads safe local attempt log files.

```sh
target/debug/luthor status --config /private/luthor/config.json
target/debug/luthor show TASK_ID --config /private/luthor/config.json
target/debug/luthor logs TASK_ID --config /private/luthor/config.json
target/debug/luthor logs TASK_ID --attempt ATTEMPT_ID --config /private/luthor/config.json
```

Pause writes a stop request for the task's latest attempt. Reconcile changes local state and may read GitHub to verify source or PR evidence; it does not launch a worker. For a task with no attempt, reconcile observes unfinished source operations. With an attempt, it checks process/receipt and PR evidence. Resume launches a new attempt and therefore requires explicit `--execute`.

```sh
target/debug/luthor pause TASK_ID --config /private/luthor/config.json
target/debug/luthor reconcile TASK_ID --config /private/luthor/config.json
target/debug/luthor reconcile TASK_ID --attempt ATTEMPT_ID --config /private/luthor/config.json
target/debug/luthor resume TASK_ID --config /private/luthor/config.json --execute
```

Use pause only when you intend to stop that task. Reconciliation can leave a task held when the available evidence is incomplete or conflicting; inspect `status` and `show` before deciding what to do next. Do not interpret silence as proof of a hung process: status warns after five minutes without observed output and does not kill the worker.

## Troubleshooting

- **`dispatch held: pass --execute to authorize GitHub writes`**: no dispatch occurred. Add `--execute` only when an authorized live action is intended.
- **`resume held: pass --execute to authorize worker launch`**: no worker launched. Use the explicit flag only after inspecting the paused task and its evidence.
- **A task is held or capacity remains reserved**: inspect `status` and `show`, then run `reconcile` if you intend to refresh evidence. Do not delete state or retry based on process absence alone.
- **Discovery returns no candidates**: check Project membership, issue state, marker type and exact value, assignees, milestone, source repositories, and repository mappings.
- **GitHub reads or identity checks fail**: verify `gh` is installed and the authenticated account has the required access and matches the configured identity. Luthor does not define credential fallbacks.
- **Push or PR verification fails**: check SSH access for the configured `push_remote`, allowed head repository, author, base branch, and exact issue link. An agent exit or PR claim is not sufficient evidence.
- **Offline Cargo commands cannot resolve dependencies**: the locked dependencies are not cached locally. Populate Cargo's cache through your approved environment before repeating the commands.

For state layout, migrations, and configuration details, see [Configuration and state](dev-docs/config-and-state.md). For what validation has and has not established, see [WP05 validation](dev-docs/wp05-validation.md).


### Continue after a natural worker exit

`retry` is an explicit authorization for one existing task in `attention`, naming
its latest completed, released attempt. It requires a natural exit with no signal
or stop intent, rechecks the private receipt and registered process absence, then
reads the source claim and exhaustive open-PR list again. It never reassigns the
issue, creates another task or worktree, or retries on daemon startup.

```sh
luthor retry TASK --attempt PREVIOUS_ATTEMPT --config /private/luthor.json --config-revision corrected-budget-512 --actor acoliver --reason 'Correct unsupported worker budget from 1024 to 512' --execute
```

The new attempt uses the current private config's `resume` executable and argv.
Its session, task branch, worktree identity, source rules, mappings, assignment
login, state/worktree roots, and capacity must remain unchanged. Only the two
worker command templates may change. Give the new attempt a revision different
from the previous attempt's revision. Keep both current templates valid, including
`--max-tool-calls 512` instead of `1024`; `-1` also means unlimited. The original
selection and previous attempts remain unchanged, including an old unsupported
argument. The private `retry_authorized` evidence records the actor, reason,
previous plan/config, new plan/config, fresh source/claim and PR absence, and reservation ID in the
same transaction as the new launch intent and reservation.

`--execute` is required. Missing or uncertain receipts/processes, pending stops,
PR errors/ambiguity/presence, claim or identity drift, conflicting worktrees,
invalid configuration, and unavailable capacity refuse the continuation. An
uncertain launch keeps its new reservation and must be reconciled; running the
same command again does not launch another worker. No SQLite edits or worker
wrappers are needed.

Normal `resume` retains its original stopped-attempt and selection-configuration
requirements. It does not adopt a retry's changed revision. A revised retry that
is subsequently paused cannot be resumed through that original-config path.
Another natural exit may be explicitly retried with a different revision and
fresh proof, but there is no automatic retry loop. The process-absence check covers
registered processes and tracked descendants, not deliberately untracked escaped
children. A reused PID or boot change fails closed.

For a historical Darwin attempt that failed during native CLI startup on
`--max-tool-calls 1024`, add `--revalidate-terminal-exit` immediately before
`--execute`. This requires the exact reconciled exit-code-2 receipt, the matching
single native startup JSON diagnostic, empty stderr, consistent original
registrations, no tracked descendants, and current absence of both recorded PIDs
and groups. The audit records this separate terminal proof without changing the
old boot identity. Ordinary historical exits stay held because escaped runtime
workers cannot be excluded by a receipt and group probes alone. See
[terminal startup revalidation](dev-docs/config-and-state.md#darwin-boot-identity-and-terminal-startup-revalidation)
for the proof and refusal boundaries.

## Rust quality gates

From the repository root, use **`cargo xtask ci`** for the same offline, locked
policy enforced on Ubuntu and macOS. See [the xtask guide](xtask/README.md) for
the compatible toolchain, deliberate dependency fetch, absolute workspace-local
Cargo/fixture directories, strict measurements and executed contracts.
All structural limits apply to existing and new code, with no exceptions;
refactor code that exceeds them.
Keep cwd stable for Unix stop-socket fixtures; do not use an external `/tmp`
workaround or weaken production path validation.

### PR provenance and issue-closing references

Every initial worker launch, stopped-session resume and audited natural-exit
retry requires both references on separate complete lines in the PR body:

```text
Tracker-Issue: https://github.com/OWNER/REPO/issues/N
Fixes #N
```

The tracker repository and issue number come from the verified task selection.
When the tracker and code repositories differ, the closing line is instead
`Fixes OWNER/REPO#N`, referring to the **tracker** repository. The exact
`Tracker-Issue` line remains required for supervisor provenance matching;
`Fixes` alone cannot satisfy it. Author, claim, base and head checks are unchanged.

GitHub closing keywords in PR bodies only automatically close issues when the
PR targets the code repository's **default branch**. The maintained Luthor base
is `work/luthor-issue-to-pr-daemon`, while this repository's default is
`bootstrap/luthor-base`. These references improve linking but cannot by
themselves guarantee closure after merge to the maintained base. The operator
must continue verifying merged delivery and closing remaining issues until
branch policy is deliberately aligned; this does not change the configured
base, repository default, branch protection or safety checks.
