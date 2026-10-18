# Issue 6: structural and coupling debt remediation

## Re-read against the current base

Tracker: https://github.com/vybestack/llxprt-luthor/issues/6

Issue 6 owns the initial measured structural outliers and module feedback edges
from issue 2, not exceptions for new code or failures of format, parsing, Clippy,
tests or safety contracts. The issue/repository and its assignment to `acoliver`
were verified before editing. This work started on `main` at
`a3a0b884652d291e2f3087fa2c83a081631f22a5`, with a clean task branch.

The issue's description anticipates `xtask/debt.json` and
`xtask/measurement.json`, but that is historical, not the current design.
Commit `3bdd41fd5119786eb83257e153c5b507a280ed8a` already decomposed the original
responsibilities, removed the remaining ledger and unused measurement snapshot,
and replaced exception-based enforcement with unconditional default limits.
Immediately before that commit, the ledger contained **78 entries**, all owned
by issue 6: 35 function-length, 17 cyclomatic, nine file-length, eight module-size,
five cognitive, one type-size, one method-count, and two cyclic-edge entries.
Earlier configuration remediation is documented in
[issue 12's report](issue12-config-validation.md).

Fresh `cargo +1.98.0 xtask measure` on the starting tree produced **11,188
measurements, zero over-limit measurements and zero cyclic edges**. There are
no remaining exception ceilings to lower or stale entries to remove. Do not
reintroduce the obsolete ledger or an automatically rewritten snapshot. The
current default limits are stricter than the original allowances and remain
unchanged.

## This change: one owner for attempt-history interpretation

The CLI's `show` function was exactly at its 80-effective-line ceiling. Its
history reader loaded and validated attempts and launch sessions, while the
presentation code separately selected latest/active attempt metadata (including
repeated active-attempt selection).

The private CLI attempt-history read model now owns that cohesive responsibility:
loading ordered attempts, validating every persisted launch identity, retaining
the latest recorded session, and selecting the latest attempt/outcome and reserved
attempt. `show` retains presentation and process-observation responsibilities;
`history` retains evidence, PR, worker-build and terminal-history summaries.
This is not a forwarding facade or a new product API. SQL ordering, JSON fields,
launch-plan validation, error propagation, reservation ownership, redaction and
read-only behavior are unchanged. The module follows the existing crate-internal
CLI helper visibility and depends directly on the CLI error leaf.

Fresh before/after measurements (effective lines, not physical lines):

| Measurement | Before | After | Default limit |
| --- | ---: | ---: | ---: |
| `src/cli/show.rs::root::show:function_lines` | 80 | 62 | 80 |
| `src/cli/show.rs::root::display_observation:function_lines` | 58 | 51 | 80 |
| `src/cli/show.rs::root:module_lines` | 184 | 159 | 600 |
| `src/cli/show.rs::file:file_lines` | 211 | 187 | 800 |
| `src/cli/history.rs::root:module_lines` | 222 | 187 | 600 |
| `src/cli/history.rs::file:file_lines` | 226 | 191 | 800 |
| New attempt-history module/file lines | — | 32 / 59 | 600 / 800 |
| New attempt-history aggregate type lines/methods | — | 17 / 3 | 400 / 20 |

The final scan has **11,212 measurements, zero violations and zero cyclic edges**.
Across production, tests and xtask, the largest measured values remain within
all seven defaults: file 786/800, function 80/80, cyclomatic 25/25, cognitive
27/30, aggregate type lines 264/400, methods 20/20, free-function module 591/600.
No metric ceiling or policy was changed.

For the original production responsibility families, the largest current file
measurements are supervisor 445 (`processes.rs`), state 400 (`continuation.rs`),
CLI 280 (`observation.rs`), coordinator 353 (`coordinator.rs`), and worktree 332
(`git.rs`), each against 800. These are fresh family maxima, not a claim that
renamed/split historical symbols have a one-to-one measurement identity.

## Behavioral regression coverage

Two new offline regressions exercise the public CLI operation against temporary
persisted state; both passed before the production extraction and after it:

- `cli::attempts::newest_attempt_without_launch_retains_previous_session_but_not_previous_outcome`:
  attempts with equal creation timestamps retain insertion order; a reservation
  without a launch intent reports the newest attempt but does not inherit an
  earlier exit outcome; the prior recorded session is retained; output bytes
  come from the reserved attempt, not an old log; reading creates no launch
  intent and exposes no private prompt.
- `cli::attempts::attempt_sessions_outcomes_and_output_remain_task_and_reservation_scoped`:
  independent tasks retain their own sessions/outcomes; an unlaunched task
  receives no other task's history; released attempts retain history without
  appearing to have active output; reads preserve reservation statuses.

Existing CLI tests still cover malformed/foreign launch plans (including older
plans), chronological ordering, active process display, unsafe logs, cached PR
proof, typed observations, redaction, and byte-for-byte read-only state/artifact
preservation under the coordinator lock. No existing assertion or test was
removed, weakened or ignored.

During characterization, the first fixture incorrectly attempted to reserve a
second attempt before releasing the first. SQLite's unique reservation constraint
correctly rejected it. The fixture now completes/releases each prior attempt
before creating the next; the constraint and assertions are unchanged. An initial
parent-scoped helper declaration also produced coupling-policy findings; the
final helper uses the same crate-internal visibility as the other private CLI
modules, without changing the coupling scanner or its zero-cycle policy. Raw
failed and successful logs are retained under `tmp/issue6/`.

## Validation and confinement

Rust **1.98.0** (`88d9e12ae`, macOS aarch64), stable task-worktree cwd:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
cargo +1.98.0 fetch --locked
cargo +1.98.0 test --offline --locked --test cli --test cli_boundaries -- --test-threads=1
cargo +1.98.0 xtask policy
cargo +1.98.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity -D clippy::type_complexity
cargo +1.98.0 xtask ci
```

All build/cache/temporary data and raw measurements/logs stay in ignored `tmp/`.
Existing fixtures retain relative state/socket paths; no external `/tmp` alias
or production Unix-socket validation change is used. Focused validation passed:
**37 CLI tests, six CLI-boundary tests, structural/coupling policy and strict
workspace Clippy**.

The full unchanged gate was attempted **once**, in its own detached session, and
exited **101** in the workspace test stage. Formatting, xtask tests/fixtures,
strict Clippy and the locked workspace build passed. The only failing test was
`supervisor::pr_repair::retry_guards::scheduled_repairs_do_not_release_capacity_with_corrupt_or_non_natural_exit_evidence`:
`tests/supervisor/pr_repair/typed_retry.rs:31` unwrapped `ReadyTimeout` while
starting a real supervisor, before receipt verification or the corrupt-evidence
assertions. The unchanged readiness handshake has a five-second wall-clock
bound (`src/supervisor/readiness.rs`). That supervisor suite otherwise reported
**242 passed, one failed, one ignored** (the ignore is pre-existing). This code
path does not use the CLI history reader changed here.

The exact failing test passed on a focused rerun (**one passed**, 16.23 seconds):

```sh
cargo +1.98.0 test --offline --locked --all-features --test supervisor \
  supervisor::pr_repair::retry_guards::scheduled_repairs_do_not_release_capacity_with_corrupt_or_non_natural_exit_evidence \
  -- --exact --test-threads=1
```

Per the task's timing-failure instruction, the full gate was **not repeated**;
no timeout, threshold or assertion was changed. The gate failed fast, so later
test binaries and the final exact-contract execution stage are not claimed as
passed. Raw evidence is `tmp/issue6/ci/{log,exit}` and
`tmp/issue6/supervisor-readytimeout-rerun.{log,exit}`. Both results are recorded
in the PR body. Hosted Ubuntu/macOS full gates must decide and remain required
before any merge. OCR review is skipped per Andrew, 2026-10-09.

No xtask implementation, contracts, Clippy settings, workflow, dependency,
timeout, assertion, safety rule or branch protection is changed. No source
checkout, operator config/state, worker executable or other task was edited.
