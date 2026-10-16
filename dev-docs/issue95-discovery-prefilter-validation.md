# Issue 95: filter project history before REST confirmation

## Scope

Discovery previously confirmed every configured Project issue over REST before
checking whether it was open, unassigned and ready. Closed issues retained the
ready marker and made idle REST cost grow with completed history.

The Project query now requests state, complete label/assignee connections, and
milestone alongside existing repository/issue identities and Project fields.
Read-only selection, targeted selection and daemon discovery filter that evidence
using each source's own repository, marker and milestone rules before REST.
Only surviving candidates receive exact-identity REST confirmation. REST evidence
remains authoritative for selection; claim, reconciliation, retry and launch
checks are not replaced by the Project snapshot. Dependencies are evaluated only
for confirmed current candidates, with their existing live prerequisite reads.

Duplicate identities, bad pagination, unsupported configured Project marker
fields, malformed summaries and incomplete connections still fail explicitly.
External read failures retain existing candidate/source isolation and discard
partial sources. No conditional REST cache or ETag behavior was added.

`ProjectItem.summary` is optional only for legacy persisted retry evidence and
identity-only direct references. Fresh Project responses require all queried
fields; discovery rejects a missing summary instead of falling back to REST.
Legacy evidence can still deserialize without inventing eligibility or changing
the separate REST authorization checks.

## Regression coverage

- 0, 1, 90 and 250 closed ready issues over multiple pages and repeated cycles:
  zero per-issue REST reads, no candidates and no hidden read failures.
- A ready open unassigned issue amid 250 closed items: one direct confirmation
  per discovery cycle; a changed REST assignee still prevents selection.
- Closed/assigned/wrong-label/wrong-milestone items are filtered at every selection
  entry point. Project-field markers filter before REST without needing a label.
- A closed prerequisite is read for the current candidate, while unrelated closed
  issues and dependencies belonging to closed former candidates are not read.
- Missing, malformed and truncated summaries fail without REST fallback.
- The actual query requests the complete eligibility fields; legacy serialized
  references still support exact-identity direct reads.
- Eligible Project evidence cannot bypass a failing pre-claim REST read or cause
  assignment. Existing successful claim contracts still require both REST reads.
- Execute-cycle `github.rest` is identical for 0, 1 and 100 closed ready items,
  with no per-issue endpoints, assignment or launch over repeated cycles.

## Validation

Rust **1.98.0**, stable repository cwd and absolute worktree-local `CARGO_HOME`,
`CARGO_TARGET_DIR` and `TMPDIR` under ignored `tmp/` are used throughout. Relative
state/socket paths retain the repository's Unix-socket fixture semantics.

- Focused discovery, eligibility, Project, dependency, claim and GitHub adapter
  integration suites: **104 passed** (`tmp/issue95/focused-5.log`).
- Idle-cycle report regression: **passed** (`tmp/issue95/idle-cycle.log`).
- Unchanged structural policy and strict workspace/all-target/all-feature Clippy:
  **passed**. Policy findings during development were fixed in code/fixtures;
  no ceilings, lint settings, contract selectors or checks were changed.
- A broader focused call including the long coordinator suite exceeded its
  600-second shell-call budget. Its 26 claim tests passed and coordinator tests
  had no reported assertion failure before tool termination; that incomplete
  run is not counted as a pass. Evidence: `focused-4.log` and
  `focused-timeout.txt` under `tmp/issue95/`.
- Full unchanged `cargo +1.98.0 xtask ci`: **passed (exit 0)**, including format,
  scanner tests, strict Clippy, workspace build, complete serial workspace tests,
  structural policy and every required behavioral contract selector. It ran
  detached without shortening any gate or timeout. Evidence:
  `tmp/issue95/full-ci/log` and `tmp/issue95/full-ci/exit`.

OCR review is skipped per Andrew 2026-10-09. Hosted CI runs the same gates and
remains required before any merge.
