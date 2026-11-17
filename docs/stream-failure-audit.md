# Stream rejection scenario audit

The default output is a scenario report, not a count of generic errors. It starts
at reviewed stream-owner rejection/termination APIs, keeps their guards and
source-local effect paths, and groups repeated cause codes across sites. A small,
reviewable catalog adds semantic cause/effect explanations and priorities.

```sh
python scripts/audit_stream_failures.py --root . \
  --json /tmp/stream-scenarios.json --markdown /tmp/stream-scenarios.md
```

This writes scenario JSON (schema 2), a Markdown review report and the complete
supporting AST inventory at `/tmp/stream-scenarios.raw.json.gz` (raw schema 1).
`--raw-json PATH` chooses the retained evidence path. The original inventory is
still available explicitly:

```sh
python scripts/audit_stream_failures.py --root . --raw-inventory \
  --json /tmp/stream-observations.json --markdown /tmp/stream-observations.md
```

The launcher builds only the standalone Rust analyzer with `--offline --locked`;
its locked dependencies must already be cached. `CARGO_TARGET_DIR` is honored;
use the approved build volume. The analysis crate is outside production host and
module dependency graphs. Python postprocessing uses only the standard library.
Directly invoking the Rust binary continues to produce the raw inventory.
Neither stage contacts the running host, opens sockets, loads plugins, changes
runtime policy or modifies source files.

## Reading the default report

The reviewed catalog covers 26 cause/effect groups, with explicit variants and
critical/recoverable/unresolved proposals. It is selective, not an assertion that
there are only 24 possible failures. Expected client teardown is separate from
failure triage. Missing required evidence downgrades a catalog entry to unresolved.

The automatic decision list starts at known RTSP, stream-session and NVHTTP sink
APIs. Generic `Err`, `?`, logs, warnings, panics and arbitrary methods named `close`
are not counted as stream rejection scenarios. Shared sink implementations are
supporting machinery, not separate causes. Source excerpts from the catalog can
supplement AST blind spots such as enum-based connection-close outcomes; a source
file's SHA-256 must agree with the raw scan before using such an excerpt. If the
working file has changed, a local `git show` of the recorded commit is accepted
only if its bytes also match the recorded digest.

The automatic list distinguishes:

- `source_confirmed_rejection_sites`: explicit negative RTSP reply or a call to a
  reviewed stream-owner API with a retained local path to a concrete state or
  response effect. This is a source fact under the recorded guards, not a claim
  of runtime execution, compiler proof, successful remote delivery or policy
  correctness. Central directives and callbacks still condition the effect.
- `uncertain_candidate`: dynamic cause, untyped/ambiguous dispatch, or no established
  local terminal effect. These remain a separate queue, not confirmed causes.
- `mixed_confirmed_and_uncertain_variants`: per-variant evidence remains separate.

Repeated specific reason codes are grouped by component and effect scope;
different phases, predicates and snapshots remain visible as variants. Generic
reasons such as `dependency_rejected` keep their branch context because they do
not identify a distinct root cause. The automatic list receives a reviewed
category only when all its anchors are covered by the corresponding catalog case.
A source-confirmed rejection may still be classified `unresolved`: knowing that a
branch stops a stream does not establish whether doing so is necessary.

P0 prioritizes security/ownership fences; P1 prioritizes recovery and admission
behavior; P2 prioritizes missing contract/propagation evidence. None of these
labels authorizes demotion. Keep authentication, encryption, required selected
media, ownership persistence and native retirement checks intact.

## Replaying saved evidence and comparing snapshots

Raw JSON or gzip reports are immutable inputs to the summarizer:

```sh
python scripts/summarize_stream_failures.py \
  --report baseline=/tmp/stream-observations.json \
  --report policy=/tmp/stream-policy.raw.json.gz \
  --json /tmp/compared-scenarios.json --markdown /tmp/compared-scenarios.md
```

`scripts/stream-failure-audit/scenarios.json` is a human-reviewable semantic
catalog. `--catalog PATH` selects an alternate catalog. IDs define scenario
identity; selectors match AST observations or hash-verified function excerpts.
Overlapping primary anchors and missing evidence are reported. Call paths carry
source positions and confidence. Their name/annotation edges do not establish
that errors escape catches or retries, cross module IPC, or reach the wire.

## Reviewing centralized decision authority

To inspect structural references to the common policy handler, supply its symbol
(or an exact function ID from raw JSON) in a checkout that contains it:

```sh
python scripts/audit_stream_failures.py --root . \
  --decision-symbol amaterasu_module_sdk::StreamFailureHandler::decide \
  --json /tmp/policy-scenarios.json --markdown /tmp/policy-scenarios.md
```

A same-function reference does not prove dominance: an early return may reject a
request before the handler, or a caller may ignore the directive. Lower layers
should carry the original facts to the final failure decision rather than repeat
severity policy. Raw structural labels do not prove compliance or a bypass.
Unmatched symbols fail with exit 2. Findings never enforce or change policy.

## Limits and validation

Raw scanning covers local Rust under `crates/` and `modules/`, including available
provider repositories. Tests, examples, build scripts, fixtures, vendored code and
symlink targets are excluded. All non-test platform/feature branches are retained
together. File digests, exclusions, missing providers, parse/I/O errors and Git
provenance are retained. Source reads are not an atomic snapshot.

The sink registry and catalog are incomplete. Native/ABI, generated code, macros,
OS/hardware, selected installed packages, replacement plugins, workers, queues,
callbacks and dynamic IPC need further source/runtime qualification. Unselected
raw evidence remains a backlog. Exit 0 means the requested reports were produced;
coverage gaps and missing catalog evidence can remain explicit. Exit 2 means
invalid options, unmatched policy symbols, parse/read/write failure or a failed
analyzer build.

```sh
cargo fmt --manifest-path scripts/stream-failure-audit/Cargo.toml --check
cargo test --manifest-path scripts/stream-failure-audit/Cargo.toml --locked -- --nocapture
cargo clippy --manifest-path scripts/stream-failure-audit/Cargo.toml --all-targets --locked -- -D warnings
python scripts/test_summarize_stream_failures.py
```

The Rust fixture measures recall/false positives for labeled syntax constructs.
Python tests verify sink specificity, causal-effect requirements, deduplication,
uncertain origins, evidence hash checks and preservation of raw inputs. An
end-to-end test runs the actual analyzer/default launcher against this checkout
and checks real rejection causes (streaming disabled, failed lifecycle journal,
invalid capture binding, and RTSP backlog refusal or its newer backpressure replacement), while rejecting promotion of
generic processing errors and optional warning origins into confirmed stream
rejections. These are regression examples, not measurements of total scenario
recall, criticality accuracy or real client playback.

For a separate policy worktree, set `AMATERASU_AUDIT_TEST_ROOT=/path/to/policy-worktree`
when running the Python suite; only source is read there, and tool builds remain
in `CARGO_TARGET_DIR`. The suite must not require an old rejection branch to
remain after its deliberate replacement by backpressure.
