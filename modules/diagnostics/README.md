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
`invalid_caller`, `caller_limit`, `counter_limit`, `counter_overflow`, or
`sequence_exhausted`.

At most 64 callers can create state, with caller identities bounded to 128 bytes.
Reads do not allocate caller slots. Caller slots and counters persist until module
restart, and are not evicted. All records, counters, sequences, and cursors are
volatile; clients must discard cursors after a process restart, which does not
have a durable epoch. Payload replies fit the runtime's 16 KiB budget. Retries
are not idempotent: a cancellation after dispatch can still leave an append or
increment committed. Counter and record writes are separate operations.

The library does not inspect environment variables, files, host logs, process
state, credentials, or network traffic. It does not export to disk/network,
redact caller strings, attach timestamps, aggregate across callers, capture
crashes, implement tracing spans, or supply a diagnostics UI. Callers should
submit only data intended for this journal. These are unimplemented parity
features, not implied collection capabilities. Native modules remain trusted
code; API grants are not a native-code sandbox.
