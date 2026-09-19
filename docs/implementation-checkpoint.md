# Completed application lifecycle checkpoint

The application, catalog and launch lifecycle increment was resumed and completed
on 2026-09-19. This checkpoint records the implemented contract and its local
verification; it is not a release or a claim of complete Vibeshine parity.

## Implemented contract

- `modules/application` advances literal-argv preparation and reverse undo across
  managed ticks, with explicit command deadlines and retained process ownership.
- `modules/catalog` validates and stores the canonical preparation configuration,
  including defaults, bounds and rejection of unknown nested fields.
- `modules/launch` forwards canonical preparation, waits for application startup
  or failure, and retains the session lease until application cleanup is terminal.
- `scripts/test_launch_workflow.py` covers ordered preparation, reverse undo and
  failed-preparation rollback through the signed catalog/session/application
  composition.

The application wire contract adds optional `preparation: []` to launch. Each of
at most eight steps contains `run` and an optional `undo` command. A command
contains `executable`, optional `args`, `environment`, `cwd`, and `timeout_ms`
(default 10000, allowed 1 through 30000). Commands use literal argv and a cleared,
explicit environment.

Application states are `preparing`, `running`, `stopping`, `cleanup_failed`, and
terminal `exited`, `launch_failed`, or `cancelled`. Every successfully spawned
preparation command with a configured undo owes that undo, even if the command
later fails or cancellation arrives. Undo starts only after the contained
application process tree has settled and runs in reverse order. Failed cleanup
retains the pending undo for explicit retry; launch therefore keeps the session
lease and does not report terminal completion. Command polling does not sleep or
wait in the shared managed-module tick.

## Verification

The completed increment passed the focused application, catalog and launch test
suites, the locked full Rust workspace, strict workspace Clippy and formatting.
The signed launch workflow passed its catalog-backed preparation and rollback
cases. The platform-module regression passed with asynchronous application
termination and terminal-state polling. Affected application code also passed
Windows MSVC and Linux GNU cross-target checks and strict Clippy; these compile
checks do not establish runtime or hardware compatibility.

## Remaining parity work

Preparation and cleanup obligations are retained by the live application service,
not durably reconstructed after a host crash or component reload. Windows graceful
quit, launcher handoff, reconnect/resume, durable cleanup watchers, production
per-application policy and Windows/Linux runtime qualification remain open. See
the [parity ledger](parity.md) for the broader display, input, media and real-client
acceptance gates.
