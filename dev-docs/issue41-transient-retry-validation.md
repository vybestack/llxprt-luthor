# Issue #41: bounded transient worker-attempt retries

## Contract and scope

Issue #41 was re-read in `vybestack/llxprt-luthor`; it is open and assigned to
`acoliver`. The task started clean on the mapped base commit
`4984e409eadd4927d7ba52adc8480d73bdfd1e35`, on
`luthor/task-a1a4f92b103be470bb37fd8a5158614e`. GitHub identity was verified as
`acoliver`. No source checkout, real worker executable, private Luthor state or
configuration, or other task worktree was edited.

The worker contract was inspected through `gh` at
`vybestack/llxprt-code-rs@9cc8677aa8270d810a32fcafb4a2c0f52778175e`.
Its terminal envelope and agent/adapter mappings expose transport-specific
codes with exit code 5, so a worker dependency issue is not needed. Classification
uses the complete bounded stdout JSON object and matching session/exit status,
not last-line matching or message text. Unknown contracts fail closed as
unclassified, with no automatic retry.

Policy defaults disabled. Opt-in execute scheduling enforces total task-attempt
count, configured transient classes, and a persisted first-observation backoff.
It calls the existing audited operator retry path, with the same receipt,
process, ownership, GitHub identity, claim, PR absence and capacity checks.
The original task/worktree/session and historical records stay unchanged.
Safety/upstream failures are audited and returned; uncertain launch reservations
stay held. Human-review outcomes are distinct in reports and cached views.

## Behavioral regressions and focused checks

- Terminal contract unit tests cover all three exact transport codes, session
  and exit disagreement, malformed/multiple/non-JSON output, quota/permanent and
  generic errors, terminal verdicts, and human handoff refusals. Message markers
  alone cannot authorize continuation.
- Five supervisor regressions exercise real private worker fixtures: two bounded
  automatic continuations through restart, preserved history and dirty progress,
  no reassignment/worktree duplication, durable backoff, capacity hold, disabled
  policy, class filtering and distinct cached human-input reporting, safety
  refusals and held uncertain launches, and counting explicit operator retries.
- Config tests verify default/round-trip behavior and reject missing bounds,
  zero backoff, empty/duplicate/unknown classes, and unknown fields. An early
  test caught serde's unit-variant unknown-field acceptance; disabled is now an
  empty struct variant so the strict validation genuinely rejects those fields.
- Focused unit, config, supervisor and existing retry CLI suites pass. Structural
  policy and strict Clippy pass; no suppressions or debt ceilings were changed.

## Validation environment

Rust 1.98.0 with absolute `CARGO_HOME`, `CARGO_TARGET_DIR` and `TMPDIR` under this
worktree's ignored `tmp/`. Supervisor fixtures retain relative state roots to
satisfy the repository's Unix-socket guidance. Logs/evidence are under
`tmp/issue41-evidence/` and `tmp/issue41-ci-2/`.

The full unchanged `cargo +1.98.0 xtask ci` is run in its own detached session.
The first background launch produced no log or exit file and its PID was absent
at the next poll; that attempt is preserved under `tmp/issue41-ci/`. Relaunching
in a fresh directory with time for the child to establish its session worked.
The gate passed structural policy, formatting, xtask tests, strict Clippy, and
all-target build, then exited 101 on one existing timing-bounded supervisor test:
`supervisor::pr_repair::semantic_conflict_hold_survives_reconcile_and_result_changes`.
The fixture's unchanged 200 × 20 ms receipt poll in
`tests/supervisor/execution.rs:154` elapsed; the assertion at
`tests/supervisor_support/stop_views/completion.rs:28` reported
`fixture receipt missing: worker stdout`. The other 185 supervisor tests,
including all five new regressions, passed. This unrelated PR-repair fixture uses
dispatch/reconcile directly, not automatic retry scheduling.

The exact focused rerun passed without edits (1 test, 4.98 seconds):
`cargo +1.98.0 test -p luthor --test supervisor --all-features --offline --locked supervisor::pr_repair::semantic_conflict_hold_survives_reconcile_and_result_changes -- --exact --test-threads=1`.
Its output is retained at `tmp/issue41-evidence/timing-rerun.log`. Following the
authorized timing-only exception, the entire gate was not rerun. Later all-target
suites, doc tests and runtime contract probes were not reached by this failed
full invocation; hosted CI must decide the complete unchanged gate.

Tests, contracts, thresholds, timeouts, required checks and safety rules are
unchanged. Hosted CI remains required before any merge; OCR review is skipped
per Andrew 2026-10-09.
