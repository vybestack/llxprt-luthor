# Issue #56: complete daemon exits before applying active PR observations

The coordinator already reconciles process/receipt proof at the start of a
cycle. The gap was a worker exiting during the subsequent GitHub reads: the
active monitor applied that observation and returned before reconciling the
new receipt. Retry launch-time absence or an active repair lookup could then
leave a successful worker held without an attempt outcome.

The active monitor now rechecks the exact attempt after its claim/PR reads,
before applying any active-stage hold or stop decision. If the attempt is no
longer running, that result goes back to the shared reconciliation path. A
verified exit continues through the existing repair-result and fresh completion
lookup logic in the same cycle. No old lookup is reused and no exit, process,
claim, PR identity or repair-tip proof is bypassed. Live-worker hold/stop
behavior remains intact.

## Behavioral regressions

`tests/daemon_completion/` drives the real dispatch/retry/daemon CLI with real
supervised workers and a local bare remote served by an isolated SSH fixture.
It never calls manual `reconcile`. Both retry-created PR and existing-PR repair
scenarios run with three exit orderings:

- exit before cycle startup;
- exit during a lookup that returns absence;
- exit during a lookup that returns an open PR.

Each verifies `pr_complete`, exit code 0, released capacity, no pending attempt,
no stop request, exactly one completion proof for the current attempt, exit
before completion lookup before PR verification, and PR head equal to the new
local/pushed tip. Repair also verifies exit before `repaired` result before
completion lookup, retaining the earlier attempt's distinct verified PR proof.
Retry retains its launch-time absent lookup without letting it decide exit.
A subsequent daemon cycle leaves completion idempotent; assignment is written
only once.

Both regressions failed against the unchanged base production files (exit 101)
and pass with the fix. Base failure evidence is in
`tmp/issue56/baseline-regressions.log`; production files were restored immediately
after that deliberate negative control.

## Validation (2026-10-13, macOS)

Rust **1.98.0 (88d9e12ae 2026-08-18)**, stable worktree cwd, absolute confined:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
export RUSTUP_TOOLCHAIN=1.98.0
```

Relative fixture state roots preserve production Unix-socket bounds. No gate,
contract, lint, structural ceiling, timeout or assertion was weakened.

- `cargo +1.98.0 xtask policy`: pass.
- Strict workspace/all-target/all-feature Clippy: pass.
- `cargo +1.98.0 test --offline --locked --test daemon -- --test-threads=1`:
  10 passed (including both regressions with all three orderings).
- Supervisor `live_`: 11 passed; `natural_exit_`: 8 passed;
  `pr_repair`: 37 passed, serial/offline/locked.
- **Full unchanged `cargo +1.98.0 xtask ci`: pass, exit 0**, in one shell call
  with timeout 5400 seconds, 15:21:01–15:59:14 UTC. Includes policy, fmt, scanner
  fixtures, strict Clippy, build, all-target serial workspace tests, doctests and
  every exact required contract. Log: `tmp/issue56/ci.log`; exit: `ci.exit`.

An earlier combined daemon + entire supervisor focused shell hit its 600-second
tool timeout after the daemon target passed and while supervisor tests were
still running (no reported test failures). This was not a full gate. Evidence:
`tmp/issue56/focused.log` and `focused-timeout.txt`. Bounded focused selections
passed; the later full unchanged gate completed successfully, including the
entire supervisor target (149 passed, its existing child helper ignored).

OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same unchanged
gates and remains required before any merge.
