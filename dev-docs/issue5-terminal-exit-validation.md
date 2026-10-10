# Historical startup exit can now continue without replacing task history

Related tracker: [issue #5](https://github.com/vybestack/llxprt-luthor/issues/5).
Delivery: [PR #4](https://github.com/vybestack/llxprt-luthor/pull/4), stacked on
`work/luthor-issue-to-pr-daemon`. Issue #2's quality-gate work is untouched.
No live retry or dispatch was run during this implementation.

## Safety evidence

Read-only inspection of a copied SQLite database/WAL found original task
`task-2d4d9c90c78b4596896981c69456b32f` in `attention`, with latest attempt
`attempt-bf8bfc3b03cbdc7b971b65ca0e5b69fb` completed, exit code 2, no signal,
no stop signals, and a released reservation. Its recorded child/group is 91106;
its supervisor/group is 91016. There are no recorded descendants. Current probes
of 91106, 91016, -91106 and -91016 each returned ESRCH.

The original receipt records 190 stdout bytes and zero stderr bytes. Stdout is
exactly the native `max-tool-calls` startup rejection for 1024 and this task's
session. The configured native binary independently reproduced exit 2 and the
same diagnostic in a synthetic session without creating files. Its source places
limit validation before session, profile/backend, profiling and tool execution.
This specific failure cannot have launched tool descendants that escaped the
recorded groups. Generic runtime exits cannot use this path.

The historical identity stays
`{ sec = 1790533213, usec = 116017 } Sun Sep 27 15:20:13 2026`. A current read was
`usec = 238774`, after the previously observed 220969. Current boot-session UUID
was `7379D9DB-543D-4D87-819E-086CEDBF1EF1`. No comparison of those microsecond
values, reconstructed UUID or boot-continuity assertion authorizes continuation.
The proof depends on the configured trusted native CLI startup contract, not an
attestation of arbitrary programs that imitate its output.

The operator command revalidates these facts itself, including fresh source,
claim, worktree, authenticated author, capacity and exhaustive PR absence.
A new attempt, reservation, launch intent, source/claim snapshot, PR observation
and terminal-exit authorization are committed together. The old receipt and
registration/gate payloads are included in the new audit and rechecked in the
transaction. Every old attempt row, intent and evidence payload stays unchanged;
new PR evidence belongs to the new attempt. Source claim, task/session,
worktree/branch history and unique issue ownership stay with the original task.

## Verification

All checks ran serially using the independent XS1000 target
`.bootstrap-issue3/continuation-target-20261001`:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo test --offline --locked --all-targets -- --test-threads=1` | 270 passed, zero failed, three existing ignored |
| `cargo clippy --offline --locked --all-targets -- -D warnings` | Passed |
| `cargo build --offline --locked --release` | Passed |
| `cargo test --offline --locked --release --test retry_cli -- --test-threads=1` | Two passed against the actual release CLI |
| Release `luthor --help` and `git diff --check` | Passed |

Tests cover successful handoff for both historical microsecond strings,
exact diagnostic/session/receipt checks, byte-preserved history, corrected argv,
single launch and the changed-revision worker gate. The ordinary 25-case refusal
matrix also runs against historical handoff. Additional tests reject live/reused
child/supervisor PIDs, surviving groups with reaped leaders, tracked escaped
process evidence, missing gate/receipt evidence, wrong exit or log content,
symlinked logs, paginated PR uncertainty and task-state conflict at authorization.
No enforcement or assertions were weakened.

The initial test-first compile failed because the explicit authorization field
was not implemented. An initial full-suite run using a symlink fixture root was
rejected by existing worktree safety checks. That run is retained; unchanged
checks passed with physical short-path XS1000 fixtures. Synthetic fixture workers
and GitHub readers are test doubles, not production worker wrappers.

Evidence and raw logs are retained under
`/Volumes/XS1000/acoliver/projects/llxprt-luthor/.bootstrap-issue3/continuation-verification-20261001/`.
`historical-release-cli.json` includes the actual synthetic authorization and
GitHub read trace. Before/after SHA-256 manifests match for all 11 original private
config/state/attempt files. The original receipt SHA-256 is
`680313e48e0b9874866f90bf63bf0b52ddc21c051d5484c715d42b3d2bede5ed`.

## Historical release verification

The release checks above used the binary at
`.bootstrap-issue3/continuation-target-20261001/release/luthor`. Its recorded
SHA-256, `8974d089ff670408e1927b387cdc3e960d16581caf6f0a46f54707a6bb750fd1`,
identifies a build from the older `a60ffeff` base. These results and the recorded
receipt are historical evidence only. They do not establish the state of a
current task, worker configuration, or checkout.

The foreground command recorded with that verification is obsolete. Do not run
the original issue #2 task retry or reuse its old binary, attempt, or config.

For a future retry that has an authorized operational need, use the supported
`luthor retry ... --revalidate-terminal-exit` flow documented in the [README](../README.md)
and [config and state guide](config-and-state.md). Gather fresh evidence for the
task, attempt, checkout, configuration, and authorization before executing a
retry. No current task retry was performed for this documentation update.
