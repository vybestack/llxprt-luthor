---
affected_files: []
cycle_number: 1
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-30T00:36:22Z'
reviewer_agent: deepthinker
wp_id: WP04
---

# WP04 review: rejected

Reviewer: deepthinker
Mission: luthor-issue-to-pr-daemon-01M3N2VK
Reviewed HEAD: 0a16b8922dfaba315aef47c754b5ecdb6cc5dc5a
Implementation range: 420741b..0a16b89, covering all committed WP04 production and test changes. Status-only commits do not change the reviewed product tree.
Audience: the WP04 implementer and mission coordinator.

WP04 must return to planned. Four blocking findings remain within T009/T010 and the PR-proof, local-control, process-safety and recovery requirements.

## R1: A valid receipt bypasses recorded descendant checks (P1)

Evidence: src/supervisor.rs:1914-1943 loads and validates tracked_descendant evidence only inside the missing-receipt branch. The receipt-present path at src/supervisor.rs:1943-2020 validates the receipt/logs and original child group, then calls reconcile_verified_exit without examining tracked_descendant. src/state.rs:1404-1415 records the exit and releases the reservation.

Mechanism: a known, recorded descendant can remain alive outside the original child group while the direct child exits and the supervisor writes a valid receipt. The original group probe can return ESRCH, so this path accounts the attempt and releases its slot despite the recorded live process. Malformed descendant evidence is also ignored when the receipt exists. This contradicts NFR-003 and the WP04 process-escape/fault scope. The cooperative-agent boundary excuses arbitrary undetected escape, not an already recorded descendant.

Tests/supervisor.rs:2022-2105 covers live/malformed descendant evidence only after deleting the receipt. Its live identity is the test process itself, so it does not exercise a worker descendant escaping its group with a valid receipt.

Remediation: include task/attempt-scoped recorded descendant identity and real OS absence checks in the receipt-present accounting path before reservation release or PR completion. Hold on live, malformed or uncertain recorded descendant evidence. Preserve existing receipt/outcome and identity checks; do not fabricate termination or signal bare PIDs.

Verification: use a real worker descendant in a separate group/session with closed inherited output descriptors so a valid receipt can be produced. Record its identity, prove it remains live, and assert held state, retained reservation, zero PR completion and zero replacement launch. Independently account/reap it and prove that reconciliation then succeeds. Also cover malformed descendant evidence with a valid receipt. No requirement to prove absence of arbitrary untracked hostile processes is added.

## R2: The documented recover command is rejected (P1)

Evidence: src/main.rs:188-208 requires args.len()==11 and args[10]=="--execute". The documented command at src/main.rs:60 has ten arguments after recover, with --execute at index 9. Tests/cli.rs:1-33 tests only the missing-execute rejection.

Reproduction against the current compiled binary:

    cargo run --quiet -- recover task --attempt attempt --config /must/not/open --actor operator --reason 'receipt lost' --execute

Result: exit 1, "expected TASK --attempt ID --config PATH --actor LOGIN --reason TEXT --execute". Inserting an arbitrary argument before --execute instead reaches "recovery configuration unavailable", proving that the parser accepts an unused positional argument and rejects its advertised syntax. Neither probe reaches GitHub or opens a state store.

Remediation: parse the advertised ten-argument form and reject extra/unrecognized arguments while retaining the execute gate before configuration/state/GitHub access.

Verification: add an actual binary-level authorized recovery test that uses the exact help syntax and a disposable lost-receipt fixture with fake read-only GitHub adapters. Assert audit evidence, unknown exit status and correct task/reservation result. Retain the missing-execute test and add an extra-argument rejection test.

## R3: Successful telemetry recovery is never recognized as terminal by scheduling (P1)

Evidence: src/state.rs:1320-1338 commits a released reservation and pr_complete task but leaves the attempt lifecycle telemetry_lost and outcome NULL. The pr_complete exemption in ensure_dispatch_capacity requires lifecycle completed, non-NULL outcome and attempt_exit evidence at src/state.rs:632-645. pending_attempts at src/state.rs:554-560 also selects every telemetry_lost/NULL-outcome attempt. inspect/reconcile's active-reservation predicate at src/state.rs:575-581 requires launch_intended and reserved, so a released recovered attempt cannot pass it.

Mechanism: a recovered PR-complete task still consumes an unresolved-task capacity unit. At capacity 1, dispatch remains blocked despite a proven absent worker and a matching verified PR. Restart also routes the recovered terminal attempt back through missing-receipt reconciliation and reports it held. This is a disagreement between the new recovery transition and the existing state readers, not a reason to invent an exit status.

Validation: executed the exact ensure_dispatch_capacity and pending_attempts SQL extracted from src/state.rs against an in-memory matching-PR recovery state. Result: reserved=0, unresolved_tasks=1, pending_attempts=[('task','attempt')]. No project state database was changed. Tests/supervisor.rs:2286-2353 checks the recovered phase/proof across reopen but never checks capacity or startup classification.

Remediation: teach terminal-attempt/scheduling readers to recognize audited, released telemetry_lost recovery with its required evidence, including the PR-complete outcome, while retaining NULL exit status and fail-closed handling for incomplete or forged recovery. Do not change uncertain reservation gates or automatically resume the recovered task.

Verification: extend both recovery outcomes through reopen/startup. For recovered PR completion at capacity 1, assert no pending live/unaccounted attempt, no held restart report for that completed task, and admission of a different eligible issue without relaunching the recovered task. Keep absent-PR recovered work explicitly held as specified and make its accounting semantics consistent. Missing audit, unresolved processes or transaction failure must still block unsafe admission.

## R4: Production PR reads discard all check summaries (P2)

Evidence: src/github/pull_request.rs:186-187 reads only the PR detail endpoint; parse_evidence unconditionally assigns checks: Vec::new() at src/github/pull_request.rs:354. There is no check/status read. src/pr_evidence.rs:225-228 persists this empty value and src/cli.rs:433-485 exposes it. Tests/pr_evidence.rs:27-41 and tests/cli.rs:236-267 manually supply red/pending strings rather than exercising the production reader.

Mechanism: every real completed PR is shown with checks=[], even when failed or pending checks are available. The acceptance requirement to record/report checks as advisory evidence is unimplemented. The current synthetic tests only show that manually populated check strings do not gate completion.

Remediation: fetch/normalize available check summaries for the observed PR head and retain them in durable proof and cached status/show output. Distinguish unavailable check evidence from an observed empty set. Keep draft/red/pending checks advisory; do not require green checks or weaken any checks.

Verification: drive GhPullRequestReader through a fake executable with failed and pending head checks, verify that the evidence survives completion and reopen into status/show, and prove matching open draft/red/pending PRs still complete. Cover unavailable check-summary reads without inventing successful or empty results.

## Full-scope evidence

- Exact Tracker-Issue lines, positive immutable PR/repository IDs, detail number/URL agreement and exhaustive pagination: src/github/pull_request.rs:195-242,245-282. Existing tests/claim.rs:165-240 covers absent, multiple links, exact URL, malformed detail and later-page errors.
- Stored task mapping/worktree branch and configured/current author matching: src/pr_evidence.rs:52-98,107-132,158-231; tests/pr_evidence.rs:64-143 and tests/supervisor.rs:893-941.
- Fresh claim validation uses direct issue and Project enumeration: src/coordinator.rs:208-220; src/claim.rs:65-154. Natural/stopped completion refuses stale assignees: tests/supervisor.rs:1241-1365. Recovery rechecks claim and worktree around PR observation and quiescence: src/coordinator.rs:250-311.
- Missing receipts cannot be overridden by automatic PR completion: tests/supervisor.rs:1073-1163. Registered child/supervisor groups and recorded descendant PIDs use actual OS ESRCH checks for missing-receipt recovery: src/supervisor.rs:1703-1747. The real supervisor-reap quiescence test is tests/supervisor.rs:2110-2146.
- Explicit recovery requires nonblank actor/reason, fresh PR evidence and repeated quiescence inspection; audit keeps exit status unavailable: src/coordinator.rs:234-357 and src/state.rs:1114-1341. Wrong PR head/author and stale claim retain reservations: tests/supervisor.rs:2504-2596.
- Recovery writes are transactional. Both absent and matching-PR release-trigger failures roll back evidence/lifecycle/reservation/task state across reopen: tests/supervisor.rs:2357-2447.
- Existing fault coverage includes claim ambiguity, duplicate Project items, source changes, worktree partial state, launch/gate loss, log/write errors, missing/corrupt receipts, identity contradictions and conservative restart holds. The receipt-present recorded-descendant gap is R1, not a claim that all existing fault tests are absent.
- Outcome remains separate from task completion; tests/supervisor.rs:1002-1068 and1168-1236 retain exit 7 after matching-PR completion. Draft/red-check verification does not gate on checks, but real check collection is missing as described in R4.

## Verification and limits

At reviewed HEAD, reran claim, cli, coordinator, pr_evidence and state integration suites serially: 85 passed, plus the state suite's isolated child-process invocation passed. Log: tmp/wp04-deepthinker-20260929/current-focused.log. cargo fmt --check and cargo clippy --locked --offline --all-targets -- -D warnings passed.

Inspected existing local supervisor logs: tmp/wp04-independent-688240b/supervisor.log and its exit marker show 57 passed, 0 failed, 1 ignored. Used code inspection and those existing process-suite results instead of rerunning the full OS suite. The missing recovery CLI, recovered-capacity/startup and receipt-present descendant assertions explain why existing green tests do not establish the missing invariants. Linux execution and live GitHub acceptance were not run; they remain WP05 work.

No contracts/ artifact exists for this mission. Charter context reports no project charter; builtin review context and the mission scope/spec/plan were read. No code, manually edited status/event logs, remote writes, pushes or gate bypasses were made.

WP05 depends on WP04 (tasks.md:76-77). Keep WP05 planned while WP04 is repaired; any dependent implementation started elsewhere must incorporate the corrected WP04 product tree before its acceptance run.
