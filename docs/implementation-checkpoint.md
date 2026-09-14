# Paused implementation checkpoint

Implementation was paused at the user's request on 2026-09-13. This is a WIP
checkpoint, not a release or a claim of completed parity.

## Last verified increment

Capture geometry integration passed 630 Rust tests (the workspace run plus the
final video-module rerun), host and Windows/Linux strict Clippy, formatting,
Windows/Linux all-target compilation, and six signed integration suites:
input, native media, RTSP, stream session, public launch, and platform services.
The parity ledger records the evidence and outstanding hardware/client gates.
These results predate the unfinished application changes below.

## Interrupted work

- `modules/application`: literal-argv preparation and reverse undo commands,
  advanced across ticks with retained child ownership and deadlines.
- `modules/catalog`: canonical storage/validation of preparation configuration.
- `modules/launch`: forwarding canonical preparation, waiting for preparation,
  and retaining session ownership until application cleanup finishes.
- `scripts/test_launch_workflow.py`: signed ordered-preparation/reverse-undo and
  failed-preparation rollback coverage. Added tests have not been run.

The agreed application wire contract adds optional `preparation: []` to launch.
Each of at most eight steps contains `run` and optional `undo` commands. A command
contains `executable`, optional `args`, `environment`, `cwd`, and `timeout_ms`
(default 10000, allowed 1 through 30000). Commands use literal argv and a cleared,
explicit environment. Every successfully spawned run owes its paired undo,
including failed or cancelled runs with possible side effects. Undo runs in
reverse order after contained process trees have settled.

Planned application states are `preparing`, `running`, `stopping`,
`cleanup_failed`, and terminal `exited`, `launch_failed`, or `cancelled`.
Failed cleanup retains pending undo for explicit retry; it must not release the
session lease or report terminal completion. Long commands must never sleep or
wait in the shared managed-module tick.

## Resume verification

At pause, `cargo check --workspace --all-targets --offline --locked` fails because
application lifecycle integration is incomplete, including an unresolved
`crate::lifecycle` import and launch patterns missing the new `preparation` field.
The diagnostic log is `/private/tmp/amaterasu-paused-check.log` (temporary local
evidence, not included in Git). Inspect the current source before resuming.

Finish the application, catalog, and launch contract together; run focused
lifecycle/ownership tests and the new signed launch workflow, then repeat the
appropriate workspace, cross-target, and signed regressions. Do not interpret
the earlier geometry test results as validation of this unfinished increment.
