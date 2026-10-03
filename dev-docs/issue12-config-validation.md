# Issue 12: configuration-validation debt

Scope: behavior-preserving private-helper extraction in `src/config.rs` and
characterization tests in `tests/config.rs`. Public types, JSON, worker flag
allowlists, native tool-budget validation, template rendering and secret checks
are unchanged. Sources still validate before mappings, mappings before initial,
and initial before resume. Each item completes before the next item's checks.

## Fresh measurements

Measured with `cargo xtask measure` before editing production code and after
extraction (rustc/cargo 1.98.0, macOS). These are fresh measurements, not values
from historical `xtask/measurement.json`.

| Key | Before | After | Ordinary limit |
| --- | ---: | ---: | ---: |
| `src/config.rs::root::Config::validate:cognitive` | 34 | 6 | 30 |
| `src/config.rs::root::Config::validate:cyclomatic` | 32 | 11 | 25 |
| `src/config.rs::root::Config::validate:function_lines` | 91 | 20 | 80 |
| `src/config.rs::root::validate_command:cognitive` | 41 | 9 | 30 |

All extracted helpers meet ordinary limits (maximum cognitive complexity: 24
for `validate_worker_option`). Removed exactly these four debt entries; no other
debt entry decreased, and all remaining entries and `xtask/owners.json` are
unchanged. Umbrella #6 remains open and is not completed by this change.

## Behavioral coverage and validation

- The first six new characterization tests passed against the original code
  before extraction: 23 config tests passed. Final coverage adds duplicate-check
  ordering and initial/resume credential redaction: all 25 config tests passed.
- Pins exact `ConfigError::Invalid` messages, first-error ordering, source and
  mapping required fields, marker/milestone cases, repository/remote syntax,
  both argv parsers, literal rendering, and budget boundaries/duplicates.
- All 17 pre-existing config tests retained, including JSON round-trip,
  template, secret and argv cases.
- Both exact retry CLI contracts passed individually:
  `explicit_retry_cli_uses_private_config_and_never_reassigns_or_reselects` and
  `historical_terminal_exit_cli_preserves_original_evidence_and_launches_corrected_worker`.
- Unchanged `cargo xtask ci` completed with exit 0: policy, fmt, scanner fixtures,
  strict Clippy, build, serial all-target tests, doc tests, and every required
  exact contract. `git diff --check` passed.

The first completed full CI run encountered an unrelated transient supervisor
identity observation: `detached_same_binary_dispatch_records_gate_and_worker_receipt`
observed `Held { reason: "supervisor identity unavailable" }` instead of completion.
Its unchanged exact test passed immediately, then the entire unchanged CI command
passed on rerun. An earlier policy attempt caught excess root-module test lines;
new characterizations were grouped in a nested module without dropping coverage
or changing limits. Tool calls have a 120-second cap, so the complete CI run used
a detached shell with confined log and exit-status files.

All validation used absolute worktree-local `CARGO_HOME=.../tmp/cargo-home`,
`CARGO_TARGET_DIR=.../tmp/target`, and `TMPDIR=.../tmp/fixtures`, with stable repo
cwd for Unix socket fixtures. Ignored `tmp/issue12/` contains fresh scanner JSON,
config/retry logs, full CI output (including the transient failure), and exit
statuses. Ubuntu and macOS hosted quality jobs must also pass on the PR; local
macOS results do not stand in for Ubuntu validation.
