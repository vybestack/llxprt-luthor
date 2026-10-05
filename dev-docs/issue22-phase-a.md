# Issue 22: initial amendment and gated launch are implemented; gates remain blocked

The isolated issue-20 checkout now includes state authorization, coordinator inspections, the `amend-undispatched` CLI, amended supervisor/worker binding, natural-exit reconciliation and matching-PR completion. The earlier phase-A-only status is obsolete. None of these changes has been applied to the live queue, configuration or worktrees.

## Preserved contracts

Ordinary `continue-undispatched` uses the exact saved launch plan. The amendment removes exactly one adjacent `--branch`, `luthor/<task-id>` pair from the first never-dispatched plan. It preserves the original launch bytes, task revision, prompt, reservation and prior history. Authorization appends `initial_branch_removed` evidence and an `initial_branch_removal_seal` intent; dispatch uses `AmendedDispatchProof` tied to the audit sequence.

The audit supports saved prompt versions and the explicit future-template correction policy. Removing the branch pair from future initial/resume configuration does not authorize a saved task's resume. `NeverDispatchedContext::plan()` and `saved_launch_plan()` still expose the original plan; `effective_plan()` is an explicit amended-plan view.

Natural-exit observation and matching-PR completion retain their separate binding contracts. Terminal observation cannot authorize redispatch. The ordinary launch, CLI and PR-complete paths remain covered by the existing tests.

## Three safety fixes

1. `supervisor/worker.rs::verify_launch_worktree` now requires the exact recorded HEAD and clean tracked/untracked state for the first attempt. Its callers enforce this at supervisor startup, coordinator READY release, supervisor gate forwarding and immediate worker pre-exec. Later resume launches and already-run observation keep their descendant-tolerant checks.
2. `supervisor/storage.rs::inspect_never_dispatched_storage` recognizes the hidden `.<attempt>.receipt.tmp` namespace, including suffix bytes. Existing files, broken symlinks and directories cause refusal before coordinator audit or dispatch. Inspection never deletes or adopts them.
3. Both telemetry-loss recovery transactions in `state/exits.rs` refuse amended task history inside an immediate transaction before any durable write. Audit, seal or typed amended-dispatch evidence is sufficient to refuse, including partial or intervening proofs. Missing-receipt recovery retains the held task, reserved slot and original history until a typed amended telemetry-loss contract exists.

The hermetic regressions use real gated subprocesses for late tracked dirt, untracked files and descendant commits in both ordinary and amended paths. Clean workers still execute. Separate tests preserve already-run dirty/descendant observation and exercise actual operator recovery with absent and matching PRs, followed by observation and startup reconciliation without new rows or unassignment. Transaction tests cover stale and intervening amendment proofs; existing unamended recovery tests remain required.

The hidden receipt tests create real regular-file, broken-symlink and directory conflicts. This macOS filesystem rejects non-UTF8 filenames, so a unit test supplies raw non-UTF8 suffix bytes directly to the production filename classifier. It does not skip that classification check.

## Verification

Rust `1.98.0` is used with locked offline Cargo builds. `TMPDIR`, `CARGO_HOME` and `CARGO_TARGET_DIR` are confined to this checkout's `tmp/f20`, `tmp/cargo-home` and `tmp/target20`. The current run's logs and exit files are `tmp/f20/safety-*`; older phase-A and dual-template logs are historical.

The existing `tmp/f20/safety-verify-driver.log` records full, documentation, formatting, strict Clippy, protected-path, and diff checks with exit 0. The policy check exits 1 only for 21 inherited stale-debt entries; there are no non-stale findings, and no debt or enforcement files were changed. These results do not constitute a green full gate.

## Remaining blockers

Policy still exits nonzero on 21 inherited stale-debt findings, with zero non-stale findings. Debt, scanner, thresholds, test enforcement and CI files are unchanged. The full gate is not green.

Saved #12 resume remains blocked. Future-template correction records configuration provenance for later selections; it does not rewrite the saved resume contract or authorize its dispatch. Issue #23 tracks that work. This run makes no live queue action and does not rerun tests that read live state.
