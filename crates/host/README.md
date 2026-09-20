# Development host

`amaterasu probe ROOT [CONTROL_MESSAGE_JSON]` runs the existing bounded lifecycle
probe. `amaterasu serve ROOT` keeps the independently loaded native modules alive
in a supervised worker until stdin EOF or an explicit shutdown command.
`amaterasu service ROOT` starts the same verified development lifecycle without
accepting console commands. On Windows it registers the `Amaterasu` SCM service
entry point and stops on SERVICE_CONTROL_STOP or SERVICE_CONTROL_SHUTDOWN. Its
loaded runtime runs in the active WTS interactive user session, never on the
Session 0 service desktop. Logoff, disconnect, and user switches terminate that
worker; the SCM service waits for the next active session. When launched outside
the SCM, stdin EOF is a test-only stop signal.

The development console accepts one `amaterasu-contracts` command per JSON line
on stdin and returns one contract response per line on stdout, in order. Unlike
probe batches, arrays are not accepted. The endpoint identity is always
`amaterasu.console`; package grants determine authority. Example input:

```json
{"op":"request","service":"amaterasu.state/1","payload":{"action":"get","key":"example"},"timeout_ms":2000,"operation_id":"console.example"}
{"host":"shutdown"}
```

Service requests wait for their result while all libraries continue ticking.
Other commands, including subscribe/publish and hook commands, return their direct
runtime response. State held by modules survives between lines. The host shell
contains no application, session, discovery, HTTP, or other product policy.

Input lines including newline are limited to 64 KiB. There are eight queued input
lines plus the current command and a bounded reader buffer. Invalid command JSON
returns `invalid_request` without restarting the worker; oversized or invalid
UTF-8 framing returns an error and closes the host. A final line without newline
is accepted. Readers apply backpressure. Clients must consume stdout replies;
this development stdin/stdout interface does not guarantee liveness if its output
consumer stops reading.

The worker ticks and checks library health approximately every 10 ms, including
while idle, and reports an internal heartbeat every 100 ms. Startup, heartbeat
silence and graceful teardown have five-second inactivity deadlines. During
startup the host worker reports finite ordered progress after manifest trust and
policy validation, after each artifact integrity check and after each independently
loaded component. Package verification is complete only when every manifest
artifact has reported a successful integrity check. Cumulative cold activation
may exceed five seconds while a single stalled artifact check, load, initial tick,
health check or teardown is still terminated after five seconds without progress. Service
requests use the caller timeout capped at 30 seconds, with a 35-second outer
supervisor deadline. Pipe delivery runs off the watchdog thread so a stopped
worker cannot block supervision with a full stdin pipe. Requests and shutdown are serialized; EOF drains already
queued input before teardown. Explicit shutdown returns `{"kind":"ok"}` after
teardown and discards later queued input.

Pending activation is health-tested and destroyed in an isolated process before
any console operation can run. Successful health commits selection; failed
candidates retain the known-good set. The root lock remains held throughout serve,
so staging another package requires stopping this host first. Restart selects the
same verified active package. There is no live package replacement.

On worker failure or missed liveness deadline, the supervisor kills/reaps the
worker and permits at most three restarts per serve lifetime, with 100/200/400 ms
backoff. An in-flight command receives `worker_unavailable` and is **never
replayed**. Its mutation may already have committed: reload and reconcile before
retrying. Queued commands that have not been dispatched continue against the new
worker. In-memory sessions, subscriptions, operation IDs and process handles do
not survive a worker restart; durable data remains the responsibility of its
own module. Native module faults may leave external effects; host supervision is
not transactional rollback or a native-code sandbox.

EOF/shutdown destroys modules in reverse load order and then their runtime. A
hung teardown is killed after the deadline and exits with an error. The
development console still rejects production roots, but the Windows SCM path
loads a signed production set in the active WTS user session. Before that worker
starts, SCM derives a SID-specific state root from its token, validates that no
reparse point is involved, and grants exactly that SID plus LocalSystem and
Administrators access. A machine-private migration seed is consumed once by the
first SID; later accounts receive isolated empty state rather than a copied
administrator verifier or pairing identity. The signed package manifest must
declare host-context schema 1, so legacy packages that could choose a state
directory through policy are rejected.

Run `python3 scripts/test_host_service.py` for real signed-module subprocess tests
of multi-request session state, durable restart, application cleanup on EOF,
explicit shutdown, bounded input and (on POSIX) worker crash recovery without
mutation replay, heartbeat timeout, restart-budget exhaustion and a stopped-worker
64 KiB pipe-delivery regression. Cargo and target overrides use the same environment as the
other repository integration scripts.
