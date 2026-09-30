# WP05 cross-platform test evidence

**Automated test status: PASS on macOS and Linux. Delivery PR and live five-item acceptance remain pending.** This record covers local automated checks and synthetic installed-binary tests only. It does not claim live-provider or GitHub acceptance evidence.

## Scope and evidence sources

WP05 calls for macOS and Linux checks, an installed LLxprt rs dry run with GitHub writes disabled, a runnable branch delivery and PR, and a separately authorized five-issue acceptance run. The automated checks are complete on both platforms. The installed-rs tests used a synthetic localhost provider. No delivery PR, real PR/account proof, live provider request, or five-item live acceptance is recorded.

Test behavior is defined in `tests/platform_dry_run.rs` and `tests/acceptance_evidence.rs`. Source logs and status files are under `tmp/mac-wp05/` in the lane and `tmp/wp05-linux/` in the host checkout, except the Linux post-fix log recorded by the worker in the container at `/src/tmp/wp05-linux/linux-postfix.log`.

## macOS automated checks

At lane HEAD `a34c79d` (`test(WP05): stabilize supervisor stop fixtures`), the final serial all-targets run passed. `tmp/mac-wp05/full-final.status` contains exit status `0`; `tmp/mac-wp05/full-final.log` ends with all targets passing, including 60 supervisor tests passed and 1 ignored. The command was:

```sh
cargo test --offline --locked --all-targets -- --test-threads=1
```

The supervisor fixture safety corrections addressed test-process cleanup. They did not weaken production process-group absence requirements.

The macOS formatting and strict lint checks also passed:

```sh
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets -- -D warnings
```

The lane records the formatting output under `tmp/mac-wp05/fmt.log` and strict clippy output under `tmp/mac-wp05/clippy-fixed.log`; the clippy status file is `tmp/mac-wp05/clippy-fixed.status`.

## Linux automated checks

An earlier host Linux serial all-targets run at `88ea34e` passed, as recorded in `tmp/wp05-linux/linux-tests.log` and `tmp/wp05-linux/linux-tests.exit`. That run preceded the final macOS supervisor fixture corrections.

After `a34c79d`, the worker reports a passing Linux serial all-targets run from the Linux container. The worker's log is `/src/tmp/wp05-linux/linux-postfix.log`. The command, run inside that container, was:

```sh
podman exec -e CARGO_HOME=/cargo-home <linux-container> \
  cargo test --offline --locked --all-targets -- --test-threads=1
```

The recorded test command used one test thread. The Linux image did not have `cargo-fmt` or `cargo-clippy` installed, so no Linux formatting or clippy result is claimed.

## Installed rs dry run on macOS

The installed-rs initial-turn and stop/resume tests ran on the macOS host, not Linux. The test binary was supplied through `LUTHOR_RS_BINARY` using the locally built release binary at `/Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1/tmp/rs-build/release/llxprt-code-rs`. The tests used a synthetic localhost HTTP provider, disposable private configuration and supervisor state, and a fixture checkout/worktree. Both ignored tests passed in two repetitions after commit `d300ae4`.

The initial-turn case sent one prompt and checked the observed request and receipt. The stop/resume case exercised an initial prompt, supervisor stop and reconciliation, then resumed with a distinct prompt while checking that session and worktree identity remained in use.

These are no-GitHub-write fixture tests. The provider is synthetic, the fixture PR is absent, and the tests check that no `gh-calls` record was written. They do not establish a live provider request, GitHub account identity, a real PR, or live acceptance.

## Acceptance-evidence tests

`tests/acceptance_evidence.rs` tests local PR-evidence rules using constructed data. It covers open draft PRs and pending or failing checks, mismatched author/head or missing tracker-link rejection, and five distinct fixture issue IDs paired with five distinct fixture PR IDs.

These unit tests do not prove that five actual issues were dispatched or that five real PRs exist. They do not establish repository, milestone, account, branch, tracker-link, draft-state, or check-state matches in GitHub.

## Delivery and live acceptance

**Delivery PR: PENDING.** This record contains no delivery PR reference or proof that a pushed branch targets the required base.

**Live five-item acceptance: PENDING.** No live provider run or five-item GitHub acceptance evidence is recorded. The acceptance run requires separate authorization. Once authorized and run through Luthor, record one row per distinct issue and PR with the required identities, milestone, tracker reference, target/base/head, account, draft state, and current checks. Preserve failures, drafts, and non-green checks as observed.

## Evidence boundary

The automated suite passed on macOS at `a34c79d` and passed on Linux after that revision, according to the worker's container log. macOS format and strict clippy checks passed. Linux format and clippy were unavailable. The installed-rs result is a macOS synthetic-provider dry run with GitHub writes absent. It gives no live-provider, real PR, or account proof. Delivery PR and five-item live acceptance remain pending.
