# Logging from plugins

Module records use the existing host endpoint. The runtime attributes each record
to the authenticated module principal; callers supply a short target and a
message without credentials, tokens, request bodies, or other private data.
The host applies its configured logging level and destination. Host acceptance
is not proof of file persistence: filtering and bounded runtime queues can drop
records after admission.

For Rust modules, `RuntimeClient::log` emits one record synchronously. For
latency-sensitive workers use `RuntimeClient::log_emitter`: clone the emitter
or bind a reusable target with `LogEmitter::logger`, then call `emit` from the
worker. The module's ordinary `tick` must call `LogQueue::flush(&runtime)`; it
sends at most 16 records per tick. This path has a bounded 128-record queue and
never waits for queue space. `stats()` reports queue-full, disconnected, host
accepted, and host rejected totals; it does not report downstream filtering or
loss. Drain remaining records during orderly shutdown where practical. Logging
must not determine whether capture, encoding, or playback succeeds.

For C plugins include `amaterasu_logging.h`. `amaterasu_log_emit` accepts UTF-8
bytes with explicit lengths and caller-owned scratch space of
`AMATERASU_LOG_SCRATCH_SIZE` bytes. It performs a synchronous host call; don't
call it from a real-time callback. The host table and its context must remain
live across the call. Scratch and input bytes need only remain live until return.

Targets must be nonempty and no longer than 128 bytes, and messages at most
7500 bytes. The host prefixes targets with the module principal and `::`; the
combined target must fit 128 bytes. Control characters in targets and NUL in
messages are rejected. Invalid UTF-8 is rejected by the host. Synchronous calls
can return an error; queued logging may drop records under backpressure, so use
bounded periodic summaries instead of emitting a log per frame.
