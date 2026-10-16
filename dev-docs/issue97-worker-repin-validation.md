# Issue 97: compatible worker repins for continuation

Verified tracker: https://github.com/vybestack/llxprt-luthor/issues/97 in
`vybestack/llxprt-luthor`, assigned to `acoliver`. Starting task HEAD was
`ea16c923df534b23b5a24b7e791df7dfdcfdc9af`, with clean status. GitHub identity
was verified as `acoliver` before writes. Work is confined to the assigned task
worktree and branch; no operator config/state, deployed worker, source checkout,
or live task (#38/#91) was modified.

## Contract

- `luthor repin TASK --config PATH --executable PATH --sha256 HEX --actor LOGIN
  --reason TEXT [--execute]` emits one JSON document, including refusals.
- The task must have a recorded (including explicitly adopted) pin, accounted
  exits for every attempt, no reserved capacity or cleanup intent, no registered
  process/group still present, and an available existing worktree owner.
- The supplied executable must lexically equal the current config's initial or
  resume executable and match its supplied SHA-256. Old executable files are
  not needed to validate historical provenance.
- The session's recorded format is mandatory. The requested binary must
  successfully answer `--session-state-versions` with the strict JSON object
  `{"session_state_versions":[2,3]}` (example), including the recorded version.
  No binary-name inference, alternative probe, or operator compatibility override
  is accepted. Output and runtime are bounded; build bytes are checked before
  and after the probe. A worker without this capability is refused.
- Preview opens the DB read-only and holds the existing owner lock; execute
  revalidates under the coordinator and owner locks. Both probe compatibility.
- Execution appends `worker_pin_replaced` with old/new paths and hashes, supplied
  executable, actor, reason, timestamp, historical attempt IDs, format,
  attestation, and task phase. It never rewrites plans, attempts, selection,
  dispatch files, exits or worktree changes, and never clears a safety hold.
- Retry, resume and PR repair resolve the current authorized pin while validating
  every historical attempt against its own pin era. Existing dispatch and worker
  gate hash checks still apply. Multiple repins form an audited chain.

## Behavioral coverage

Nine new integration tests exercise:

- read-only preview, complete `show` audit, restart and real resume execution;
- receipt-less/running and busy-owner refusals with unchanged persisted state;
- live registered child, supervisor and descendant refusals even when database
  accounting says completed;
- wrong/malformed hashes, symlink and lexical aliases, obsolete template paths,
  missing format, failed/malformed/unsupported worker capability responses;
- CLI one-document output and execute-time rechecks of both bytes and capability;
- refusal of unrecorded legacy pins, adoption followed by repin, unrelated holds;
- real retry dispatch of a new build and a second repin across multiple pin eras,
  preserving old selection, launch plans and dispatch artifacts;
- missing/corrupt replacement evidence and changed bytes block before dispatch;
- real PR-repair execution with the replacement while preserving the old plan.

## Validation environment and results

Rust **1.98.0**; stable repository-root cwd. All Cargo/cache/build/temporary data
remain in ignored worktree `tmp/`:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
```

Fixture state roots remain relative to that cwd, following the repository's
Unix-socket path guidance. Dependencies were deliberately fetched once using
`cargo +1.98.0 fetch --locked`; subsequent checks are offline and locked.

- Focused `cargo +1.98.0 test --offline --locked --test supervisor pin_ --
  --test-threads=1`: **28 passed**, including existing adoption/pin safety tests.
- Focused PR-repair repin test: **1 passed**, also included in the final pin suite.
- Unchanged CLI suite, serial like the full gate: **35 passed**.
- Focused CLI revalidation test: **1 passed**.
- `cargo +1.98.0 xtask policy`: passed with unchanged ceilings.
- Strict workspace/all-target/all-feature Clippy: passed.
- Formatting and `git diff --check`: passed.

The prior worker's unchanged full `cargo +1.98.0 xtask ci` completed with
**exit 0**, including all exact required contracts. Its detached log and exit
file are preserved under `tmp/issue97/ci3/`. The preceding gate (`ci2/`) caught a
real read-only CLI regression: the resolver applied repin-only session
validation even when no replacement audit existed. This was fixed by keeping
the original resolver for tasks without repin evidence, without relaxing any
audited-chain checks or changing any existing test. The unchanged CLI suite
passes all 35 tests. An additional noncanonical parallel CLI invocation hit four
existing fixture owner-lock `Busy` failures at `tests/cli/harness.rs:210`; its log
is preserved in `tmp/issue97/cli-tests.log`. The gate's unchanged serial
invocation passed all 35 (`cli-tests-serial.log`).

### Continuation after attempt-ef9d38929b9b5d743c2c28363be26542

Re-inspected the existing worktree, staged diff, tests and logs; re-read issue
97 and verified `gh api user` reports `acoliver`. Preserved the prior worker's
implementation and nine behavioral tests. Starting HEAD remains the recorded
`ea16c923df534b23b5a24b7e791df7dfdcfdc9af`; no task work was discarded.

- Fresh focused `cargo +1.98.0 test --offline --locked --test supervisor repin_
  -- --test-threads=1`: **9 passed** (`continuation/repin-tests.log`).
- Fresh unchanged full `cargo +1.98.0 xtask ci`: **exit 101**, solely on
  `supervisor::pr_repair::typed_retry::typed_transient_repair_schedules_one_bounded_relaunch_after_restart`.
  The supervisor suite reported **231 passed, 1 failed, 1 ignored**, including
  all nine new repin tests passing. Formatting, gate fixtures, strict Clippy,
  build and all earlier test targets passed. This run stopped before doc tests
  and the exact-contract stage; it is not reported as a successful full gate.
- Failure: `tests/supervisor_support/stop_views/completion.rs:48`,
  `fixture exit receipt missing`, during the unchanged initial fixture setup.
  `tests/supervisor/execution.rs:169` waits at most 200 iterations of 20 ms
  before that assertion. This fixture has no repin evidence and the no-repin
  resolver continues to use the original pin-adoption contract. Neither the
  failing test nor its wait/assertion was changed.
- Focused exact rerun: **1 passed** in 7.59 s (`continuation/timing-rerun.log`).
  Repeating with the full gate's workspace/all-target/all-feature options
  executed the same `supervisor-fe7bdc498f0b2f67` binary and passed **1 test** in
  9.32 s (`continuation/timing-rerun-exact-gate.log`).

All continuation logs and exit files are under `tmp/issue97/continuation/`.
The full gate was detached using the requested session pattern, with short
polls verifying the live process tree while output was buffered. The final
poll's `ps` returned nonzero because the completed gate PID was already gone;
its exit file and test failure output were retained and inspected. Following
the timing-failure instruction, the whole gate was not rerun repeatedly and no
timeout, threshold or assertion was changed. The PR records both the failure
and passing focused reruns; hosted CI must decide merge readiness.

The first detached launcher exited before producing
output; its absent PID/zero-byte log and relaunch evidence are preserved in
`tmp/issue97/ci/`. The requested session-detachment pattern was relaunched with
confirmation of the detached PID before the launching shell returned.

No tests, contracts, debt ceilings, lint thresholds, required checks or safety
rules were relaxed. OCR review is skipped per Andrew 2026-10-09. Hosted CI runs
the same gate and remains required before any merge.
