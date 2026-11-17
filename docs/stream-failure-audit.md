# Auditing stream rejection and termination

Run the read-only source inventory from a checkout with the provider sources
available. It uses a Rust AST, rather than matching log text or only status 503.
It does not contact a host, start a stream, change runtime policy, or load a plugin.

```sh
python scripts/audit_stream_failures.py --root . \
  --json /tmp/stream-failures.json --markdown /tmp/stream-failures.md
```

The Python launcher builds only the standalone analysis crate with Cargo's
`--offline --locked` flags. Its dependencies must already be cached; otherwise
fetch the analyzer's locked dependencies in an environment with registry access.
Use `CARGO_TARGET_DIR` for your approved build volume. The standalone crate keeps
`syn`, `quote` and the analysis executable out of production module dependencies.
You can also build once and run `amaterasu-stream-failure-audit` directly.

The central failure decision should have final authority over RTSP/network
rejection. Lower layers should report conditions to it. To inspect structural
references to a proposed central function, supply its exact function ID from the
JSON or `package_name::Type::method` (hyphens become underscores):

```sh
python scripts/audit_stream_failures.py --root . \
  --decision-symbol amaterasu_rtsp::Controller::handle_network \
  --json /tmp/stream-decisions.json --markdown /tmp/stream-decisions.md
```

This example selects a network handler for inspection; it does **not** assert
that it is already the desired universal policy handler. Prefer a full function
ID when the same type/method name exists in several modules. Multiple
`--decision-symbol` options can compare candidates. Unmatched symbols produce
exit status 2 so a misspelled policy name cannot appear to pass.

## Reviewing the evidence

The Markdown queue prioritizes stream-scope boundary candidates. The JSON is the
complete inventory: locations, source excerpts, literal reasons, enclosing
if/match/let-else conditions, function IDs, candidate caller chains, indexed
call edges, unresolved dispatch, API metadata, file digests, and Git provenance.
`--markdown-limit` defaults to 200; omissions are stated in the report. Bounds
(`--max-depth 5`, `--max-paths 3`) apply to displayed caller paths, not the source
inventory. Truncated paths are marked and the full indexed edge list remains.
Function/edge indexes refer to arrays in the same JSON report; finding IDs include
the source position and expression, so they may change after edits.

Observations distinguish explicit error origins (`Err`), `?` propagation,
`map_err`/`ok_or` conversions, negative JSON replies, wire status candidates,
failure/termination method calls, failed lifecycle assignments, false/None
returns, panic hazards, and opaque macro/module/indirect dispatch boundaries.
Constant successful `reply_status` responses are filtered. An `Err` pattern in
a caught optional fallback, a log containing “failed”, or a string containing
“503” does not create an error origin. Shutdown names can still refer to local
resources or normal client teardown; candidates must be reviewed.

Classifications are deliberately tentative:

- `critical_proposed`: an explicit `StreamIssue::critical` declaration or a
  recognized security/ownership reason. Confirm the real contract and effect.
- `superficial_proposed`: the source explicitly declares an advisory/demotion.
  Confirm a safe compatible fallback and exact lifecycle reporting.
- `normal_teardown_proposed`: `close(None)` supplies no failure reason. Confirm
  the initiating request and resource retirement.
- `unresolved`: no reliable decision follows from syntax. This includes optional
  errors whose fallbacks have not been verified.

Central-decision labels distinguish a site inside the configured symbol, a
resolved policy reference in the same function, and no resolved policy reference.
None proves dominance or compliance. A call can be in the wrong branch, ignore
the result, or report diagnostics after rejecting. Treat unreferenced RTSP reply,
ticket revocation and stop sites as a review queue for the centralized design.
Do not duplicate policy in lower layers to satisfy this inventory, and never
weaken authentication, encryption, required media contracts, ownership or native
retirement fences based on a proposed demotion.

## Coverage and blind spots

All local Rust under `crates/` and `modules/` is considered, including available
nested provider repositories. Failure/termination and wire names use a finite
vocabulary; custom wrappers, status constants, non-JSON encodings, silent thread
exit, deadlocks and other error conventions can still be missed. Expand the
fixtures when adding conventions. The analyzer retains non-test platform/feature
branches together. It excludes explicit `#[test]`/`#[cfg(test)]` declarations,
test directories/files, examples, build scripts, fixtures and vendored sources.
Every scanned file has a SHA-256 digest; excluded paths, parse/I/O errors, missing
submodule source and skipped symlinks are listed. Re-run after concurrent edits;
the scan is not an atomic source snapshot. Successful parsing is not coverage of
all runtime scenarios.

Name/receiver matching is not compiler resolution. Same-name ambiguity, aliases,
trait objects, generics, callbacks, queues, workers, cancellation and IPC leave
paths unresolved. A caller chain does not prove that a returned error reaches a
shutdown: it may be caught or retried. Async/closure guards flag their separate
execution boundaries. Metadata lists advertised APIs but cannot establish a
selected provider or runtime service route. Native C/C++/ABI, generated code,
unexpanded macros, OS/hardware failures, external/replacement plugins and installed
package selection require separate inspection and runtime qualification.

Exit status 0 means the requested inventory was written without parse/I/O errors
or unmatched policy symbols. Missing providers/symlinks are explicit coverage
gaps, even with exit 0. Exit 2 means invalid options, parsing/read/write failure,
or unmatched policy symbols. Findings never fail CI on their own and never
authorize changing security behavior.

## Validation

```sh
cargo fmt --manifest-path scripts/stream-failure-audit/Cargo.toml --check
cargo test --manifest-path scripts/stream-failure-audit/Cargo.toml --locked -- --nocapture
cargo clippy --manifest-path scripts/stream-failure-audit/Cargo.toml --all-targets --locked -- -D warnings
```

The manually labeled Rust fixture measures recall of observable constructs and
false positives on negative controls. It is not a measurement of repository-wide
scenario recall or real stream criticality. Tests also verify indirect call paths,
receiver disambiguation, conditions, tentative classification, policy-reference
limits, parse failure reporting, missing provider detection, deterministic output
and CLI error behavior. These checks run with the existing component CI tooling.
