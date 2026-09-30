---
affected_files: []
cycle_number: 2
mission_slug: luthor-issue-to-pr-daemon-01M3N2VK
reproduction_command:
reviewed_at: '2026-09-30T01:54:53Z'
reviewer_agent: deepthinker
wp_id: WP04
---

# WP04 verification: changes requested

Reviewer: deepthinker
Mission: luthor-issue-to-pr-daemon-01M3N2VK
Reviewed root HEAD: d32393950716f451ac7b84554caf32a4c041570e
Checkout: /Volumes/XS1000/acoliver/projects/llxprt-luthor/branch-1
Scope: original WP04 T009/T010, mission requirements, and R1-R4 recorded in review-cycle-1.md. No additional acceptance requirements are introduced.

## Blocking: R3 remains for recovery after a prior paused attempt (P1)

The single-attempt recovery case now works. The same recovery/scheduling disagreement remains after an ordinary pause and explicit resume.

Evidence:

- src/state.rs:572 and src/state.rs:682 reject terminal telemetry-loss recovery whenever any earlier attempt has attempt_exit without exit_pr_lookup. Both predicates require that particular evidence kind regardless of how the earlier attempt ended.
- A properly accounted paused attempt records pause_pr_lookup through src/coordinator.rs:428-453, particularly line 451. It does not record exit_pr_lookup. Natural-exit accounting records exit_pr_lookup through src/coordinator.rs:472-504.
- The existing general pending-attempt predicate already distinguishes those histories with the stop-intent-dependent CASE at src/state.rs:575-577. The two new prior-attempt predicates do not use that distinction.
- src/coordinator.rs:373-392 sends pending attempts through reconciliation on startup. A released telemetry_lost attempt cannot satisfy the launch_intended/reserved predicate at src/state.rs:593-599; with its missing receipt it returns held through src/supervisor.rs:1923-1931.
- tests/state.rs:686-708 supplies a prior attempt with exit_pr_lookup, while tests/supervisor.rs:2458-2550 exercises recovery without a prior pause. Neither reproduces the failing valid history.

Mechanism: first pause an attempt with a verified stopped process group and an absent PR read, then explicitly resume. If the new attempt loses its receipt, audited recovery can prove process absence, verify a matching open PR, release its reservation and set pr_complete. Both scheduling readers nevertheless classify its earlier, fully accounted pause as unresolved because pause_pr_lookup is not exit_pr_lookup. At capacity 1, another issue cannot dispatch. Restart selects the recovered terminal attempt again and reports it held. The recovered attempt's unknown exit status is appropriate and must remain unknown.

Independent reproduction used the exact pending_attempts and unresolved_tasks SQL extracted from current src/state.rs, against an in-memory SQLite database. It produced:

```text
control: prior accounted natural exit
  active_reservations=0, unresolved_tasks=0, pending_attempts=[]
same recovered terminal task with prior accounted pause
  active_reservations=0, unresolved_tasks=1, pending_attempts=[['task','recovered']]
```

Log: tmp/wp04-deepthinker-followup-2e29efce/recovered-after-pause-sql.log. No project state database was opened or modified by this reproduction.

Reproduction command, from the reviewed root:

```sh
python3 - <<'PY'
import json, re, sqlite3
from pathlib import Path
s = Path('src/state.rs').read_text()
p = s.split('pub fn pending_attempts', 1)[1].split('pub(crate) fn active_attempt_reservation', 1)[0]
c = s.split('let unresolved_tasks: usize = self.connection.query_row(', 1)[1]
p = re.search(r'"(SELECT.*?)",', p, re.S).group(1)
c = re.search(r'"(SELECT.*?)",', c, re.S).group(1)
db = sqlite3.connect(':memory:')
db.executescript('''
CREATE TABLE tasks(id TEXT, state TEXT);
CREATE TABLE attempts(id TEXT, task_id TEXT, lifecycle TEXT, outcome TEXT);
CREATE TABLE reservations(attempt_id TEXT, task_id TEXT, status TEXT);
CREATE TABLE evidence(task_id TEXT, attempt_id TEXT, kind TEXT, payload TEXT);
CREATE TABLE intents(task_id TEXT, attempt_id TEXT, kind TEXT);
INSERT INTO tasks VALUES('task','pr_complete');
INSERT INTO attempts VALUES('prior','task','completed','exit_code=Some(0);signal=None');
INSERT INTO attempts VALUES('recovered','task','telemetry_lost',NULL);
INSERT INTO reservations VALUES('prior','task','released'),('recovered','task','released');
''')
def add(a, k, v):
    db.execute('INSERT INTO evidence VALUES(?,?,?,?)', ('task', a, k, json.dumps(v)))
add('prior','attempt_exit',{'exit_code':0,'signal':None})
add('prior','exit_pr_lookup',{'status':{'status':'absent'}})
add(None,'claim_verified','operator')
add(None,'worktree_created',{'branch':'luthor/task'})
add('recovered','telemetry_lost',{'actor':'operator','reason':'receipt lost','observed_at_unix_secs':10,'os_ids':[{'pid':12345,'boot_identity':'boot','start_identity':'start'}]})
add('recovered','exit_pr_lookup',{'status':{'status':'open'}})
add('recovered','verified_open_pr',{'id':99,'attempt_id':'recovered'})
print('natural control:', db.execute(c).fetchone(), db.execute(p).fetchall())
db.execute("UPDATE evidence SET kind='pause_pr_lookup' WHERE attempt_id='prior' AND kind='exit_pr_lookup'")
db.execute("INSERT INTO intents VALUES('task','prior','stop')")
db.execute("UPDATE attempts SET outcome='exit_code=None;signal=Some(2)' WHERE id='prior'")
print('accounted pause:', db.execute(c).fetchone(), db.execute(p).fetchall())
PY
```

Minimal correction: make both new prior-attempt accounting predicates recognize the existing stopped-versus-natural PR-evidence distinction. Preserve the required audit, released reservation, verified PR, unknown exit status, and rejection of unresolved prior attempts. Do not synthesize an exit_pr_lookup for a paused attempt or weaken process-termination checks.

Verification remains the original R3 request: extend real pause/resume and audited matching-PR recovery through reopen/startup at capacity 1. Assert that the completed recovered task has no pending attempt or held restart report, and a different eligible issue can dispatch without relaunching the recovered task. Keep absent-PR recovery held, and missing audit, live/uncertain processes and transactional release failure blocking unsafe admission.

## Findings verification

- R1 production defect resolved: src/supervisor.rs:1914-1921 validates tracked descendant records for both receipt branches; lines 2019-2039 check their live OS identities/absence before first exit accounting. Independent supervisor tests passed for valid-receipt live identities, malformed identities, and the separate-session process followed by reaping at tests/supervisor.rs:2022-2185. The separate-session test creates a real process from the test harness, not from the supervised worker; it exercises the recorded-identity gate but should not be described as proof of actual worker ancestry tracking.
- R2 parser defect resolved: src/main.rs:189-205 accepts the documented ten-argument form, rejects extras and checks --execute before opening configuration/state or invoking adapters. Binary tests at tests/cli.rs:3-74 passed. The original requested successful binary-level recovery fixture with fake read-only GitHub remains absent; the success path is currently covered by direct coordinator recovery tests instead.
- R3 partially resolved: tests/supervisor.rs:2458-2550 proves single-attempt recovery remains pr_complete after reopen, has no pending attempt, permits capacity and retains NULL outcome. tests/state.rs:712-755 holds missing recovery evidence. The valid prior-pause history above is still blocked.
- R4 production collection resolved: src/github/pull_request.rs:142-240 gathers head-SHA check runs and legacy status summaries; lines 290-305 attach available evidence. Unavailable evidence stays None, while an observed empty set is Some([]). Parsing at lines 476-486, durable proof at src/pr_evidence.rs:225-228 and cached output at src/cli.rs:465-487 preserve that distinction. Tests/claim.rs:92-232 passed real GhPullRequestReader fake-executable tests for failed/pending checks, empty/unavailable/malformed/incomplete reads, pagination and legacy statuses. The original composed adapter-to-completion-to-reopen/status/show regression is not present; current proof/output tests exercise those later stages separately. Red/pending checks remain advisory.

The absent composed R2/R4 tests are evidence limitations already named in the initial feedback. The demonstrated runtime blocker for this verdict is R3.

## Original-scope verification

Independent offline tests exercised exhaustive PR lookup, absence/error/ambiguity, exact tracker/body and target/base/head/account validation; direct claim freshness; draft/red-check proof; separate attempt exit and PR completion; durable logging/receipts; PID reuse and group/descendant uncertainty; SQLite write rollback; lost receipts; source changes and duplicates; partial/foreign worktrees; stop, resume and startup capacity safety.

Production completion call sites were traced through src/coordinator.rs:404-425,428-548,565-602 and src/state.rs:852-880. Audited telemetry recovery was traced through src/coordinator.rs:234-357 and src/state.rs:1151-1378. Matching PR completion retains original exit 7 and exposes it after reopen in tests/supervisor.rs:1002-1068. No retry, repair, review, merge or unassignment mechanism was added. Private receipt/log validation is at src/supervisor.rs:1773-1788,1803-1809,1943-1967.

## Independent verification evidence

All commands ran at the reviewed root with --locked --offline and serial integration tests:

- cargo test --locked --offline --test claim --test cli --test coordinator --test pr_evidence --test state -- --test-threads=1: 93 passed, plus one isolated state child-process invocation. Log: tmp/wp04-deepthinker-followup-2e29efce/focused.log.
- cargo test --locked --offline --test supervisor -- --test-threads=1: 60 passed, 0 failed, 1 helper ignored in the parent; the helper passed in its isolated invocation. Log: tmp/wp04-deepthinker-followup-2e29efce/supervisor.log.
- cargo test --locked --offline --lib --test config --test daemon --test eligibility --test project --test worktree -- --test-threads=1: 79 passed. Log: tmp/wp04-deepthinker-followup-2e29efce/remaining-offline.log.
- cargo fmt --check: passed. Log: tmp/wp04-deepthinker-followup-2e29efce/fmt.log.
- cargo clippy --locked --offline --all-targets -- -D warnings: passed. Log: tmp/wp04-deepthinker-followup-2e29efce/clippy.log.
- Exact production-SQL recovery classification reproduction: confirmed the R3 result above. Log: tmp/wp04-deepthinker-followup-2e29efce/recovered-after-pause-sql.log.

232 parent-suite tests passed, plus two isolated child-process runs. The failing valid prior-pause recovery history is not asserted by those suites. Linux execution and live GitHub acceptance were not performed; platform delivery and live acceptance remain separate WP05 work.

## Prompt checklist and coordination

1. Dead code: PASS. Production call-site search found expected_for_task and VerifiedOpenPr in coordinator completion/recovery, recovery in main, and recovery state commits in coordinator.
2. Synthetic-fixture test: PASS for exercised production functions. Fake executables invoke the production adapters, and database fixtures invoke the actual state readers. Composed R2/R4 limitations are explicitly recorded above.
3. Silent empty return: PASS. Check evidence uses None for unavailable versus Some([]) for observed empty, and safety uncertainties return held/errors rather than fabricated absence.
4. FR coverage: FAIL for the retained R3 behavior under FR-006/FR-008/FR-009. The current assertions omit valid prior-pause terminal recovery classification.
5. Frozen surface: N/A. The WP/spec/plan identify no frozen code files.
6. Locked decision: PASS for prohibited operations. No automatic retry/review/repair/merge/unassignment or CI-success gate was found.
7. Shared-file ownership: PASS with this coordination note. WP04 uses the accumulated root product tree of the shared lane defined in lanes.json. Its state, coordinator, supervisor, main, CLI and tests overlap prior WPs; their completion and process-safety invariants must be retained. Only WP04 status is changed by this review.
8. Production fragility: PASS for intentional fail-closed handling of external GitHub/OS/storage uncertainty; the reproduced reader disagreement is reported as R3 rather than accepted as uncertainty.

WP05 depends on WP04. Do not proceed with its acceptance run while this blocker remains. Any dependent implementation started elsewhere must incorporate the corrected WP04 tree.

No source/test code edits, GitHub writes, pushes, manually edited status/event logs, --force or gate bypasses were made. This feedback is the rationale for the guarded move of WP04 to planned.
