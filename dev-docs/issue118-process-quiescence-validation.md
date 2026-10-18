# Issue 118: identity-aware quiescent operations

Tracker-Issue: https://github.com/vybestack/llxprt-luthor/issues/118

## Verified requirements and scope

The live issue is #118 in `vybestack/llxprt-luthor`, assigned to `acoliver`.
The task began at `47b67a6056a5fd85e1a874b1671dc8325595abe9` with a clean
worktree on `luthor/task-a692663b4b67db01c706ed914b31509b`. GitHub authentication
was verified as `acoliver` before writes. The mandatory mapped PR base is `main`.
All work, dependencies, fixtures, logs and builds stay inside this task worktree.
No operator queue/state/configuration, installed worker, source checkout or other
task worktree was modified.

`state::process_quiescence` now reads the existing registered boot/start identities
using the same OS identity reader used at launch. A different readable start
identity proves that the PID no longer names the registered process. A different
boot-session UUID proves that all registered processes and group members are gone,
even when an individual PID's start identity cannot be read. Boot UUID comparison
is case-insensitive. Unknown/legacy Darwin boottime encodings cannot prove a reboot;
a different readable start identity can still prove PID reuse for such records.

A PID's absence or reuse does **not**, on its own, prove group absence. The check
still requires ESRCH for the group, or an independently observed recycled group
leader: the registered group must equal the registered PID, its current PID must
have a different identity, and it must actually lead that group. Identity is read
again to confirm the leader observation. Unix reserves a surviving process-group
ID against PID allocation; a new leader with that ID therefore proves the old
group no longer exists. A leaderless group with surviving members, a leader PID
outside the group, an unreadable identity, or a non-leader registration cannot
supply that proof and remains blocked. No process enumeration or permission-based
assumption is used to ignore unknown members. Invalid IDs and malformed JSON still
refuse; SQL/serialization failures propagate.

The change applies to every caller of the shared helper:

- Capacity resize, including registrations outside the one independently confirmed
  running attempt. Exact running-attempt observation and exclusion are unchanged.
- Worker pin replacement, both preview and execute through `pin_quiescence`.
- Native continuation reservation and begin-dispatch through its journal.

Each operation retains its separate lifecycle, receipt, reservation, ownership,
configuration and launch-authority checks. In particular, predecessor exit
rechecking can still refuse a contradictory registration on that predecessor;
this change is not permission to bypass reconciliation. Running-attempt proof,
unreconciled launches, stop/signal authorization, missing-receipt recovery and
reservation release are unchanged. Nothing synthesizes a receipt or rewrites
historical evidence.

## Behavioral regression coverage

- Four real-process resize regressions exercise a terminal task with a reused PID
  and recycled process group (all three registration kinds), matching live
  identities, a previous boot, and a live group member after its leader exits.
  They verify unchanged history, atomic refusal/audit behavior and no signaling
  of the unrelated live process.
- Nine unit regressions inject only the read-only OS observations into the real
  journal-reading quiescence path. They cover unreadable starts for every
  registration kind (including an absent group), matching/reused identities,
  surviving members, a reused PID outside its group, reboot/unreadable boot,
  legacy boot encodings, UUID case, malformed records and exact attempt exclusion.
  This makes permission/platform failures deterministic without relying on host
  permissions or weakening resize's `LaunchBlocked` refusal.
- A repin regression verifies that reused historical registrations no longer
  obstruct preview/execute and are not rewritten.
- A native-continuation regression verifies that a reused task-scoped registration
  permits reservation and real dispatch while preserving the evidence and all
  new-attempt proof gates.
- Existing live-process refusal fixtures now record the actual current boot/start
  identity instead of fabricated mismatching identities. All refusal assertions
  remain intact; these fixtures now genuinely test a matching live registration.

## Validation environment and results

Rust **1.98.0**, macOS aarch64, stable repository cwd:

```sh
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
export TMPDIR="$PWD/tmp/f"
cargo +1.98.0 fetch --locked
cargo +1.98.0 test --offline --locked --lib state::process_quiescence -- --test-threads=1
cargo +1.98.0 test --offline --locked --test capacity_resize --test historical_hold --test state -- --test-threads=1
cargo +1.98.0 test --offline --locked --test supervisor pin_replacement:: -- --test-threads=1
cargo +1.98.0 test --offline --locked --test coordinator native_continuation -- --test-threads=1
cargo +1.98.0 test --offline --locked --test cli cli::capacity:: -- --test-threads=1
cargo +1.98.0 xtask policy
cargo +1.98.0 clippy --offline --locked --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity -D clippy::type_complexity
cargo +1.98.0 xtask ci
```

Fixture state/socket paths follow `xtask/README.md`'s confined-relative guidance;
production socket limits, timeouts and assertions are unchanged. Raw evidence is
under ignored `tmp/issue118/`.

- Original implementation: the reused-PID resize regression **failed** with the
  reported `registered process may be active; reconcile first` refusal
  (`regression-red.log`, exit 101).
- Shared quiescence unit tests: **9 passed** (`unit-final.log`).
- Capacity, historical-hold and state suites: **30 + 15 + 28 passed**
  (`focused-final.log`).
- Repin-focused suite: **10 passed** (`repin.log`).
- Native-continuation suite: **9 passed** (`native-final.log`). An initial new
  fixture attached contradictory evidence to the predecessor itself and correctly
  failed its separate exit recheck. The final regression uses task-scoped history
  to isolate the shared quiescence check; exit-recheck safety was not changed.
- Running-capacity CLI suite: **7 passed, 1 failed** (`running.log`). The unchanged
  fixture timed out in its existing READY handshake, before any resize, for
  `cli::capacity::confirmed_worker_and_permanent_historical_hold_are_counted_separately`.
  Its exact focused rerun **passed** (`running-timing-rerun.log`).
- Structural policy and strict all-target/all-feature Clippy: **passed**
  (`policy-final.log`, `clippy.log`). An intermediate parent/child probe-module
  dependency failed coupling policy; consolidating the cohesive quiescence code
  removed that cycle without changing the policy.
- The full unchanged gate runs detached using the required Python `setsid` launch
  pattern. The first launch disappeared without producing output (zero-byte log, no PID
  or exit file); this evidence is retained under `ci/`. The relaunch and actual
  gate results are under `ci-relaunch/`.

**Full unchanged `cargo +1.98.0 xtask ci`: PASSED, exit 0.** This includes
structural/debt policy, formatting, gate fixtures, strict Clippy, all-target and
all-feature build/test, and every required exact contract. Representative workspace
results: 189 library tests, 30 capacity tests, 45 CLI tests, 142 coordinator tests
and 244 supervisor tests passed. The pre-existing ignored tests remain unchanged;
the gate executes its required exact contracts separately. The earlier focused
READY timeout did not recur in the full CLI suite. No whole-gate rerun was needed.

OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same unchanged
Ubuntu/macOS gates and is required before any merge. No gate, debt ceiling,
threshold, contract, required check or branch protection was lowered. No merge
or issue closure is part of this task.
