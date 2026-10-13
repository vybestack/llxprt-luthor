# Issue 51: PR repair held finished tasks and failed every cycle

## Cause

`repair_verified_prs` compared each `pr_complete` task's selection-time
configuration with the current one before it looked up the task's PR. Tasks
selected under an older revision (new optional keys, changed worker paths)
therefore failed the comparison first. The error branch held the task and
returned an error that failed the whole cycle, one more finished task per cycle,
including tasks whose PR had already merged. Held tasks count as unresolved in
`ensure_dispatch_capacity`, so queues filled up.

## Behavior now

- The saved PR is looked up first. No open PR (merged or closed) means the task
  is left alone: no configuration check, no hold, no error.
- Configuration drift on a task whose PR is open skips the task with
  `selection config differs: <fields>` (from `EffectiveConfigSnapshot::differing_fields`).
  No state changes. An unreadable lookup is skipped with its category and code.
- Safety conflicts on an open PR (changed head, author or PR id, ambiguous PR,
  claim, process, receipt or ownership evidence, unreadable health) still hold
  the task, now with `PR repair held: <stage>`. The previous generic reason is
  no longer written.
- The pass visits every candidate. A hold or skip is recorded against its task
  and does not end the pass. Only a failure to read or write state aborts it.
- The cycle prints one JSON document; skipped, held and restored tasks appear in
  its `pr_repair` object.
- Restoration (`coordinator::restore_wrongly_held_prs`, run before cleanup and
  scheduling in execute cycles) moves a task held with exactly the old generic
  reason back to `pr_complete` after `cleanup::terminal_pr::observe` freshly
  proves the saved PR closed or merged by exact identity. The qualification
  (latest hold reason, no reservation, no unfinished attempt, no cleanup intent)
  is rechecked in the update's transaction. A `pr_hold_restored` row records the
  proof; no row is deleted. There is no backward-compatibility path for other
  reasons.

## Tests

`tests/supervisor/pr_repair/history.rs` (coordinator level, real worktree and
receipt fixture) and `src/daemon/tests/repairs.rs` (whole execute cycle with a
hermetic `gh`) cover: merged PR with a different configuration (no hold, cycle
succeeds), open PR with a different configuration (skipped with reason, another
issue still dispatches), restoration of a held task with a merged or closed PR,
capacity freed afterwards, the cases restoration must refuse, and a specific
reason for genuine holds. The existing
`changed_repair_config_and_missing_previous_receipt_do_not_launch` test changed
for its configuration case only: that case asserted an error, which is the
behavior this issue removes, and now asserts the skip with its reason and an
unchanged task. Its missing-receipt case and every other assertion are as before.
