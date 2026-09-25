# In-memory diagnostics

`amaterasu.diagnostics/1` stores caller-provided structured records and unsigned
named counters. The broker's authenticated immediate `caller` determines the
namespace. Payloads cannot override it. Reads return only that caller's data;
there is no administrative cross-caller query or publication.

Requests (unknown fields are rejected):

```json
{"action":"record","level":"info","target":"capture","message":"started","fields":{"device":"display/0"}}
{"action":"query","after":0,"limit":4}
{"action":"increment","name":"frames","amount":1}
{"action":"counters"}
{"action":"status"}
```

Record levels are `trace`, `debug`, `info`, `warn`, and `error`. `fields` defaults
to an empty object and contains strings only. Targets, field keys, and counter
names contain 1–64 ASCII letters, digits, `.`, `_`, `-`, or `/`. Messages may be
empty and contain at most 512 UTF-8 bytes; at most eight fields are accepted,
each with at most 256 value bytes. A complete serialized record reserves space
for a twenty-digit sequence and must fit 2048 bytes, including JSON escaping.

Record success is `{"ok":true,"sequence":1,"dropped_total":0}`. Query success:

```json
{"ok":true,"records":[{"sequence":1,"level":"info","target":"capture","message":"started","fields":{"device":"display/0"}}],"next_cursor":1,"has_more":false,"latest_sequence":1,"oldest_sequence":1,"dropped_total":0,"resync_required":false}
```

`after` defaults to zero and means strictly after that caller's sequence. `limit`
defaults to four and must be 1–4. Continue using `next_cursor`; an empty page keeps
the input cursor. `has_more` reflects records available when the query runs.
Sequences start at one independently per caller. At most 128 records per caller
are retained; each additional append evicts the oldest and increments
`dropped_total`. If `after` predates the retained boundary, `resync_required` is
true and the page starts at the oldest retained record. The client should report
the gap and continue from the returned cursor; missing records cannot be recovered.
`oldest_sequence` is null for an empty journal. A cursor beyond `latest_sequence`
returns `invalid_cursor`. Concurrent appends can evict unread pages; each page
reports the current retention state. There is no frozen multi-page snapshot.

Increment success is `{"ok":true,"name":"frames","value":1}`. Amounts are
unsigned 64-bit integers, including zero. Counter reads return
`{"ok":true,"counters":{"frames":1}}`. There are at most 32 names per caller;
values never wrap, and overflow leaves the previous value intact. Errors are
`{"ok":false,"code":"..."}` with codes `invalid_request`, `invalid_record`,
`record_too_large`, `invalid_name`, `invalid_limit`, `invalid_cursor`,
`invalid_caller`, `caller_limit`, `counter_limit`, `counter_overflow`,
`sequence_exhausted`, and `not_ready` while logging policy hydration is pending.
The `status` action returns `{"ok":true,"ready":false}` until hydration
completes. Query and counter responses remain available during hydration and
include `ready:false`.

At most 64 callers can create state, with caller identities bounded to 128 bytes.
Reads do not allocate caller slots. Caller slots and counters persist until module
restart, and are not evicted. All records, counters, sequences, and cursors are
volatile; clients must discard cursors after a process restart, which does not
have a durable epoch. Payload replies fit the runtime's 16 KiB budget. Retries
are not idempotent: a cancellation after dispatch can still leave an append or
increment committed. Counter and record writes are separate operations.

## Host log output

The host reads `global.min_log_level` and `global.log_path` from its verified
bootstrap policy once when each worker starts. `min_log_level` accepts
`verbose`, `debug`, `info`, `warning`, `error`, `fatal`, and `none`, plus a
numeric string beginning with `0` through `6`. Names are lowercase and exact;
an unrecognized value keeps the default. The default is `debug` for a Cargo
package version with a non-`stable` prerelease suffix, and `info` for stable
versions. A `log_path` absent from configuration defaults to
`<state_root>/amaterasu.log`. Relative custom paths resolve beneath the
protected state root; parent traversal and symlink escapes are rejected.
Absolute custom paths are allowed. A path that names an existing directory,
or a path without an extension, selects a log directory. A file path selects a
`logs` directory beside the configured file and uses the file stem as the
session prefix.

Each accepted diagnostic record is still kept in its caller's bounded journal.
Records at or above the configured level are also written as JSON lines to the
worker's shared session log. Each line includes the runtime-supplied caller
identity alongside the caller-supplied target, so records from different callers
can be distinguished even when they use the same target. The file combines
records from callers, so callers must not submit secrets or content they do not
intend to make available to the host account. Log files roll at 2 MiB, retain
up to four rolled files plus the active file per session, and startup prunes old
sessions to keep at most 30
session groups. If the disk sink cannot be opened or later fails, the volatile
journal continues to work. The logger retains the current validated policy and
tries to open a fresh session on the next accepted record at or above the
configured level. Each attempt repeats the trusted path checks. Records missed
by the sink during an outage are not backfilled; if opening continues to fail,
later qualifying records trigger another attempt. A positive persisted settings
revision reconfigures the sink only when it exceeds the highest revision adopted
by this worker.

This is a host diagnostics sink, not a capture of all host or native-library
logs. Service supervisor messages on stderr and worker protocol lines on stdout
remain outside this file sink. Worker `READY` does not wait for persisted
logging hydration. At startup, diagnostics asynchronously calls
`amaterasu.host-configuration/1` with the read-only `get` action and does not
read configuration state directly. The status action reports `ready:false`
until a logging policy is selected. During this period, Record requests return
`not_ready` and are not retained or written; query, counter, and status actions
remain available. Query and counter responses report `ready:false` while
hydration is pending.

When the profile grants the provider call, diagnostics keeps readiness false
until it fetches a ready v1 document with a positive revision. Before adopting
it, diagnostics validates the configured log path, checks that the effective
path matches the configured path under the trusted state root (or the same
absolute path), validates the effective minimum level, and applies the logger's
existing trusted path checks. Only then does it configure the persisted logger
and accept records. If the provider is granted but unavailable, returns
`ready:false`, or returns malformed or invalid data, diagnostics remains
unready and retries; it does not enable the bootstrap sink as a fallback.

Profiles without this provider grant keep the injected signed bootstrap
logging settings. Diagnostics enables those settings only after the initial
service Request is denied immediately with `permission_denied`. An
`unavailable` response, a transport failure, or an asynchronous provider error
does not identify a legacy profile and never enables bootstrap logging.

The reader releases and discards its first valid ready document after startup,
then schedules a fresh GET after the retry delay before it adopts a policy.
This prevents a restarted reader from adopting an older completed GET retained
under its stable operation ID. After a valid document is adopted, later
documents reconfigure the sink only when their positive revision exceeds the
highest revision successfully adopted by this worker. Invalid refresh results
leave the current policy active. The module polls every 30 seconds after a
successful read.

The diagnostics reader uses the caller-scoped operation ID
`diagnostics.host-configuration.logging.read` for each serialized GET cycle.
If the diagnostics module restarts while its broker request is still retained,
the new reader resubmits the same service, payload, and parent and reattaches to
the pending request or unreleased result. It releases every terminal result;
failed releases remain pending for retry. The first valid ready document after
startup is discarded after release, and the reader retries the same operation
ID to start a fresh GET. `ReleaseResult` removes the broker retry record, so
the same ID can start that new GET. The broker keeps completed records for up
to 300 seconds. Its request table is in memory, so restarting the broker itself
starts a fresh GET. This relies on one diagnostics reader per broker caller;
simultaneous readers would share this ID and could race to consume or release a
result. A retained request created by an older version's numbered IDs cannot
be recovered with the new stable ID and expires under the broker's normal
retention policy.

This sink does not capture all host or native-library logs, inspect process
state, credentials, or network traffic, capture crashes, implement tracing
spans, or supply a diagnostics UI. Native modules remain trusted code; API
grants are not a native-code sandbox.
