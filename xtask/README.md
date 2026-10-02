# One offline, locked Rust quality gate

From the repository root (keep cwd stable throughout process fixtures):

```sh
mkdir -p "$PWD/tmp/fixtures" "$PWD/tmp/cargo-home" "$PWD/tmp/target"
export TMPDIR="$PWD/tmp/fixtures"
export CARGO_HOME="$PWD/tmp/cargo-home"
export CARGO_TARGET_DIR="$PWD/tmp/target"
cargo fetch --locked                 # deliberate network stage, once per empty cache
cargo xtask ci                       # same command in Ubuntu and macOS CI
```

Use Rust **1.98.0**, including rustfmt and Clippy. The CI toolchain is explicit;
local validation was run with rustc 1.98.0. Commit Cargo.lock and use locked,
offline validation. No toolchain installation is needed when it is already
available. Build and fixture directories stay in ignored workspace `tmp`.
No downloads/builds touch the private operator state or acceptance-live.

`ci` runs structural/coupling/suppression/owner policy, fmt, gate fixtures,
general strict Clippy (workspace/all targets/all features, `-D warnings`, plus
explicit denied cognitive/type complexity), locked build, all-target serial
tests, documentation tests, then every exact required contract individually.
A nonzero subprocess exit is retained. Spawn failures fail closed. Exact
contract invocations may filter *other* tests; a required test must itself
execute and finish `ok`, never ignored, renamed, absent, failed or unsupported.
The final GitHub `Luthor quality gates` job runs even after matrix failure and
requires success on **both** Ubuntu and macOS; skipped jobs are not success.

`cargo xtask measure` emits JSON without writing any baseline; `policy` and
`contracts` diagnose individual stages, using the same policy. Normal gates
never rewrite debt, ownership evidence, contracts or reports. Run `ci` before
commit/push. Do not baseline failures of fmt, tests, Clippy, unreadable sources,
parser errors, incomplete trees or unsupported source-generating macros.

## Measurement and numerical policy

`measurement.json` records token-aware initial measurements after the
behavior-preserving supervisor proof refactor. Each entry has a tightly keyed
path/symbol/metric, measured value and policy limit. Duplicate cfg alternatives
use the **maximum** per-symbol measurement; aggregates conservatively include
all alternatives. Initial scanner output is not platform-dependent.

New code ceilings: **800 effective lines/file, 80/function, cyclomatic 25,
cognitive 30** (the issue's proposals, unchanged). For file LOC, a physical line
counts once when it contains a Rust token outside a parsed attribute. Blank
lines, comments and attribute-only lines do not count. The scanner excludes
attribute token positions, not whole source lines: code before or after an
attribute on the same line still counts. Multiline attributes, inner attributes,
field/variant/parameter/statement/arm attributes and attributes in parsed
standard macro arguments follow the same rule. Doc comments are parsed doc
attributes; explicit doc strings and literal `include_str!` doc values are
metadata too. Ordinary multiline strings still count on every occupied line;
attribute-looking text inside a string is never stripped. CRLF, empty files and
missing final newlines are covered by fixtures.

Attributes remain in the syntax tree for suppression and source validation.
Both cfg alternatives count; nested cfg_attr metadata is validated regardless
of its condition. Unknown attribute macros, unknown derives, source-path
attributes and unsupported doc expressions fail rather than requiring an
unmeasured expansion. [The attribute policy](src/attribute_policy.rs) lists
supported metadata and derives. Standard derives resolve through their builtin
bindings, including raw identifiers and equivalent core/std paths. Local imports,
aliases and reexports are resolved before granting an exemption. Serde
Serialize/Deserialize and thiserror Error require Cargo's offline, locked resolution
to the supported crates.io packages and registry-only dependency trees. Renamed
dependencies are supported; path/git replacements and unresolvable external
reexports or glob bindings fail closed. A standalone metric call cannot verify
procedural dependencies and requires a workspace scan for these derives.

Serde helpers are accepted only on a record, variant or field belonging to a
verified serde derive, and their values must be literal metadata. Thiserror helpers
require a verified Error derive: error accepts only a single string literal or
transparent, while from/source/backtrace are field markers. Additional formatting
arguments, blocks, includes and other executable helper forms fail rather than
being excluded from measurement. Raw builtin metadata, used/no_std and supported
unsafe attribute wrappers retain their exemption. These known derives are measured
as handwritten source, without counting their generated implementations. The
scanner does not execute procedural attributes or infer arbitrary expansions.
Attributes inside opaque xtask token-construction macros remain input tokens,
not parsed attributes.

Function LOC retains its signature/body token measurement, excluding the
function's own attributes but including nested attributes in that body. Type
and module LOC retain their sums of function LOC. This file correction does
not change those metrics. Nested branches/loops/match arms/boolean operators
contribute to the documented traversal's complexity; cognitive nesting adds
weight. This is a conservative syntax metric, not Clippy's exact cognitive
algorithm; both independent checks enforce their ceilings. Compiled record
fixtures and an executable policy-gate fixture verify that 799 and 800 file
lines pass, 801 fails, and excluded lint suppressions still fail.

Behavior-heavy types: **400 effective implementation lines / 20 methods**,
aggregated across inherent and trait impls and files, normalizing generic
instantiations and imported aliases. Default trait methods count too. Pure
records/enums without methods are not God objects. Free-function modules:
**600 effective function lines**, including coherent inline modules. Module
and file ceilings are separate; splitting impl blocks or files cannot evade
type totals. Existing StateStore (49 methods, >1,400 implementation lines)
is conspicuous debt; next-largest production behavior type is GhProjectReader
(369 lines). Twenty methods leaves useful headroom for ordinary cohesive types
but rejects StateStore-scale concentration. Worktree's 449 free-function lines
fit 600; supervisor/CLI/coordinator modules exceed it and require decomposition
by coherent responsibilities, not arbitrary file sharding. New xtask code fits
all limits; no xtask debt exceptions were introduced.

All required production/test/xtask roots must exist and parse. Production trees
are resolved from lib/main entries; declared missing/ambiguous or orphan sources
fail. External `#[path]` and cfg_attr path variants are unsupported and fail.
Both cfg branches are traversed, never silently omitted. Source-generating
macros (including custom nested macros and include!) fail: this scanner does
not expand arbitrary Rust. A narrow set of standard expression macros and
serde_json/rusqlite expression macros is accepted as opaque input: arguments
count towards LOC, not invented expanded functions. Include_str/include_bytes
are data, not code. xtask quote!/syn Token! are parser tooling only. New custom
macros require an explicit policy/fixture review, never an automatic fallback.

The module graph resolves crate/self/super imports and qualified paths. An edge
is cyclic exactly when its target can reach its source in the complete graph.
Every cyclic edge is measured once at value 1 with new-code limit 0. The
deterministic feedback subset uses `:feedback`; the original three feedback
keys and ceilings remain unchanged. Cyclic edges outside that subset use
`coupling::<from>-><to>:cyclic_edge`. The feedback subset is computed by
accepting sorted edges unless they close a cycle in the accepted graph.
Complete reachability uses all edges, including feedback, so adding a path
behind an owned feedback edge cannot hide growth.
A newly cyclic edge needs its own reviewed ledger entry. Removing an edge or
breaking its return path makes its entry stale; same-size replacement produces
both stale and unowned keys. Acyclic growth does not create cycle debt.

The cyclic portion of `measurement.json` now includes the three already
existing non-feedback cyclic edges: `src/pr_evidence->src/state`,
`src/state->src/supervisor`, and `src/supervisor->src/worktree`. The initial
structural snapshot is retained; its file LOC predates attribute exclusion.
The nine affected file-debt ceilings in `debt.json` have been lowered to the
corrected measurements. Function/type/module and cyclic debt are unchanged.
Each added cyclic debt entry has measured ceiling 1;
no original feedback key or numeric ceiling changed. Each structural outlier
and edge is owned by [remediation issue #6](https://github.com/vybestack/llxprt-luthor/issues/6).
`owners.json` is reviewed GitHub evidence (open, assigned acoliver); ownership
is validated in Rust offline, not through a Python script or a hidden network
fallback. Refresh evidence explicitly when ownership changes. Stale entries,
removed/renamed symbols, growing ceilings, unknown owner issues and broad
wildcards fail. Improvements require an explicit reviewed ceiling decrease.

## Executed behavioral contracts

`contracts.json` links stable scenario descriptions to exact integration-test
identifiers. It covers selection (membership/milestone/marker/assignee/conflict),
no-write preview, durable intent before external assignment, changed evidence,
ambiguous writes/PR lookup, capacity and duplicate identity, uncertain launch,
worktree identity/unfinished intent, registered child gating, stop/reconciliation
uncertainty, exact verified PR identity, draft/red-check acceptance, and supported
Unix socket semantics. Tests assert persisted domain state and forbidden effects,
not source layout. General all-target tests remain mandatory in addition to these
individual executions. Existing opt-in installed-LLxprt smoke tests are not the
offline contract suite and remain opt-in; CI does not misreport them as executed.

Root clippy.toml is checked for exact settings. External Clippy/Cargo compiler
wrappers/flags/config replacements are rejected. Baseline/manifest/workflow
changes are reviewable policy changes, not an escape hatch for failing code.
Independent review is still required even when the automated gate passes.
