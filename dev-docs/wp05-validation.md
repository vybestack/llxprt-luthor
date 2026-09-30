# WP05 cross-platform test and live acceptance evidence

**WP05 delivery and the five-issue live acceptance run are complete.** The implementation delivery PR and the five issue PRs are separate. The issue PRs were open and non-draft when directly rechecked through GitHub API; their checks are a mix of failing, pending, and no reported failures. This record does not claim green CI or resolved failures.

## Scope and prior automated evidence

WP05 covers macOS and Linux checks, an installed LLxprt rs dry run with GitHub writes disabled, runnable branch delivery, and a separately authorized live acceptance run. The earlier automated checks and synthetic-provider dry run below remain distinct from live GitHub acceptance.

Test behavior is defined in `tests/platform_dry_run.rs` and `tests/acceptance_evidence.rs`. Source logs and status files are under `tmp/mac-wp05/` in the lane and `tmp/wp05-linux/` in the host checkout, except the Linux post-fix log recorded by the worker in the container at `/src/tmp/wp05-linux/linux-postfix.log`.

### macOS automated checks

At lane HEAD `a34c79d` (`test(WP05): stabilize supervisor stop fixtures`), the final serial all-targets run passed. `tmp/mac-wp05/full-final.status` contains exit status `0`; `tmp/mac-wp05/full-final.log` records all targets passing, including 60 supervisor tests passed and 1 ignored. The command was:

```sh
cargo test --offline --locked --all-targets -- --test-threads=1
```

The supervisor fixture safety corrections addressed test-process cleanup. They did not weaken production process-group absence requirements.

Formatting and strict Clippy checks also passed:

```sh
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets -- -D warnings
```

The lane records formatting output under `tmp/mac-wp05/fmt.log` and strict Clippy output under `tmp/mac-wp05/clippy-fixed.log`; the Clippy status file is `tmp/mac-wp05/clippy-fixed.status`.

### Linux automated checks

An earlier host Linux serial all-targets run at `88ea34e` passed, as recorded in `tmp/wp05-linux/linux-tests.log` and `tmp/wp05-linux/linux-tests.exit`. That run preceded the final macOS supervisor fixture corrections.

After `a34c79d`, the worker reported a passing Linux serial all-targets run from the Linux container. The worker's log is `/src/tmp/wp05-linux/linux-postfix.log`. The command, run inside that container, was:

```sh
podman exec -e CARGO_HOME=/cargo-home <linux-container> \
  cargo test --offline --locked --all-targets -- --test-threads=1
```

The Linux image lacked `cargo-fmt` and `cargo-clippy`; no Linux formatting or Clippy result is claimed.

### Installed rs dry run on macOS

The installed-rs initial-turn and stop/resume tests ran on the macOS host, not Linux. The test binary was supplied through `LUTHOR_RS_BINARY` using the locally built release binary at `/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/rs-build/release/llxprt-code-rs`. The tests used a synthetic localhost HTTP provider, disposable private configuration and supervisor state, and a fixture checkout/worktree. Both ignored tests passed in two repetitions after `d300ae4`.

The stop/resume case exercised an initial prompt, supervisor stop and reconciliation, and a distinct resume prompt while checking session and worktree identity. These fixture tests verified that no `gh-calls` record was written. They establish no live provider request, GitHub account identity, real PR, or live acceptance.

### Acceptance-evidence tests

`tests/acceptance_evidence.rs` exercises local PR-evidence rules with constructed data, including draft PRs, pending or failing checks, mismatched author/head or missing tracker-link rejection, and five distinct fixture issue and PR IDs. These unit tests are not proof of actual GitHub issues or PRs.

## Delivery and live acceptance setup

The Luthor daemon implementation was delivered separately in [Luthor PR #1](https://github.com/vybestack/llxprt-luthor/pull/1), which is open. Live acceptance used Project `PVT_kwDODYHhhs4BOTgm`, exact ready label `luthor-ready`, and milestone `0.12.0`. Each of the five issues was individually previewed as a Project member, open, and unassigned before dispatch. Only Luthor claimed and assigned the issues. Workers ran in isolated worktrees; no manual code or PR fixes were made.

After reconciliation, all five tasks were in `pr_complete`; each latest attempt was `completed` and matched `verified_pr.attempt_id`. There were zero reserved slots. All workers exited with code zero, with no signal or stop. Issue #3403 had a transient held completion-claim read, which cleared through ordinary next-daemon startup, not manual recovery.

GitHub API directly rechecked the five PRs and their source issues after Luthor produced its proof. Each source issue remained open, assigned by Luthor to `acoliver`, with the exact label and milestone. Every PR was open and non-draft, authored by `acoliver`, based on `main`, and had head and base repository node ID `R_kgDOPB5qbQ` (`vybestack/llxprt-code`). Each contained the exact tracker body line shown in the table. Task branches followed `luthor/<task-id>` and worktrees were isolated under `tmp/acceptance-live/worktrees/<task-id>`.

## Five-PR evidence table

Check state is the GitHub snapshot queried after Luthor's proof. “No failing or pending checks as queried” describes that snapshot only; it is not a claim that all required checks succeeded.

| Issue / task | Isolated worktree and branch | Attempt / worker result | Source issue | PR and immutable ID | Repository, base, head, tracker link | Author / state | Draft / checks at snapshot |
|---|---|---|---|---|---|---|---|
| [#3402](https://github.com/vybestack/llxprt-code/issues/3402) / `task-4fcdf6a2a6b9c527b50139b5e69ea918` | `tmp/acceptance-live/worktrees/task-4fcdf6a2a6b9c527b50139b5e69ea918`; `luthor/task-4fcdf6a2a6b9c527b50139b5e69ea918` | `attempt-f578d52bb1bb9dcfff650602d709b38f`; completed, exit 0, no signal/stop | Open; Project member; `luthor-ready`; milestone `0.12.0`; assigned to `acoliver` by Luthor | [#3775](https://github.com/vybestack/llxprt-code/pull/3775); immutable ID `4693871617` | Head/base repository `R_kgDOPB5qbQ`; base `main`; exact body line: `Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/3402` | `acoliver`; open | Non-draft; `Lint` and `Lint (Javascript)` FAILURE |
| [#3403](https://github.com/vybestack/llxprt-code/issues/3403) / `task-fbebebf7b263457a36c3ca262d520179` | `tmp/acceptance-live/worktrees/task-fbebebf7b263457a36c3ca262d520179`; `luthor/task-fbebebf7b263457a36c3ca262d520179` | `attempt-ebbdd7dcb6844988e47a352913f9e20c`; completed, exit 0, no signal/stop | Open; Project member; `luthor-ready`; milestone `0.12.0`; assigned to `acoliver` by Luthor | [#3777](https://github.com/vybestack/llxprt-code/pull/3777); immutable ID `4694042510` | Head/base repository `R_kgDOPB5qbQ`; base `main`; exact body line: `Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/3403` | `acoliver`; open | Non-draft; `Test`, `Lint`, `scripts 1of1`, and `Lint (Javascript)` FAILURE |
| [#3612](https://github.com/vybestack/llxprt-code/issues/3612) / `task-6629d18ab63927b6374b2cd2c75608c5` | `tmp/acceptance-live/worktrees/task-6629d18ab63927b6374b2cd2c75608c5`; `luthor/task-6629d18ab63927b6374b2cd2c75608c5` | `attempt-cfa63a0e05fb29917c17bcd21d8bce61`; completed, exit 0, no signal/stop | Open; Project member; `luthor-ready`; milestone `0.12.0`; assigned to `acoliver` by Luthor | [#3778](https://github.com/vybestack/llxprt-code/pull/3778); immutable ID `4694116684` | Head/base repository `R_kgDOPB5qbQ`; base `main`; exact body line: `Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/3612` | `acoliver`; open | Non-draft; no failing or pending checks as queried |
| [#3645](https://github.com/vybestack/llxprt-code/issues/3645) / `task-18adc36c9f544750edab60bc0628806e` | `tmp/acceptance-live/worktrees/task-18adc36c9f544750edab60bc0628806e`; `luthor/task-18adc36c9f544750edab60bc0628806e` | `attempt-2c44f6e85c340bbf20bac8d668fa9d44`; completed, exit 0, no signal/stop | Open; Project member; `luthor-ready`; milestone `0.12.0`; assigned to `acoliver` by Luthor | [#3779](https://github.com/vybestack/llxprt-code/pull/3779); immutable ID `4694286850` | Head/base repository `R_kgDOPB5qbQ`; base `main`; exact body line: `Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/3645` | `acoliver`; open | Non-draft; `Test` and `cli 3of3` FAILURE; `Run LLxprt review` IN_PROGRESS |
| [#3679](https://github.com/vybestack/llxprt-code/issues/3679) / `task-5161e26efb84be0c2e86db0f723da946` | `tmp/acceptance-live/worktrees/task-5161e26efb84be0c2e86db0f723da946`; `luthor/task-5161e26efb84be0c2e86db0f723da946` | `attempt-b9a3a88c3ad8ffb6d28b0024d6b1e373`; completed, exit 0, no signal/stop | Open; Project member; `luthor-ready`; milestone `0.12.0`; assigned to `acoliver` by Luthor | [#3780](https://github.com/vybestack/llxprt-code/pull/3780); immutable ID `4694534375` | Head/base repository `R_kgDOPB5qbQ`; base `main`; exact body line: `Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/3679` | `acoliver`; open | Non-draft; two E2E Linux sandbox checks IN_PROGRESS |

## CI and verification caveats

The check states above were observed while PRs remained open. They may change. No failing check is represented as resolved. The #3780 worker reported format, lint, typecheck, build, and 125 focused tests passing; a full rerun passed all 755 CLI test files. The repository-wide test command returned nonzero on the unrelated `providerAgnosticNaming.test.ts` scanner. Smoke testing was blocked because the `ollamakimi` profile was missing. These worker reports do not supersede the live PR check snapshot.

## Evidence boundary

The macOS/Linux automated suite, synthetic-provider macOS dry run, and constructed-data acceptance tests are local test evidence. The five rows above record separate live GitHub issue and PR evidence, with source eligibility checked before Luthor dispatch and PR/source state rechecked afterward. All five tasks reconciled to `pr_complete`, with matching latest completed attempt IDs and zero reserved slots. This establishes five distinct open, non-draft PRs with the recorded linkage and identities at the queried snapshot. It does not establish green CI, merged PRs, or resolved failures and pending checks.
