# Session history module

`amaterasu.session-history/1` owns the history SQLite database and processes
one request at a time. This gives database mutations a single ordered writer.
The database uses WAL mode. A trusted `database_path` module setting can
override its location; development profiles inject a path beneath the
package's private state directory. Production falls back to the current user's
application data directory under `Amaterasu/session-history/history.db`.
Configured database paths must be absolute file paths.

The product settings default to `enabled: true`, `ttl_days: 0`, and
`db_size_limit_mb: 0`. They are stored as version 1 settings in
`amaterasu.configuration/1`, with its product revision used for compare-and-swap
updates.

At startup, an existing central product document is authoritative. If none
exists, the module imports values from a previous SQLite `history_settings`
row, or initializes the defaults, using a revision 0 compare-and-swap. The old
row is removed only after the central document is confirmed. If configuration
cannot be loaded, the history service remains available for reads and lifecycle
calls, while new recording remains disabled and settings requests wait for
configuration.

## RPC

Management calls are accepted only from `amaterasu.management`:

- `settings_get {}`
- `settings_update {"settings":{"enabled":true,"ttl_days":0,"db_size_limit_mb":0},"expected_revision":1}`
- `list_sessions {"limit":25,"offset":0}`
- `session_detail {"session_id":"..."}`
- `timeline_open {"session_id":"..."}`
- `timeline_open {"session_id":"...","sample_limit":300}` (most recent 1–2,000 samples)
- `timeline_page {"session_id":"...","kind":"samples","cursor":{...},"limit":32}`
- `active_sessions {"limit":16}`
- `history_status {}`
- `delete_session {"session_id":"..."}`

Streaming lifecycle calls are accepted only from `amaterasu.stream-session`:

- `session_begin {"metadata":{...},"started_at_unix":0}`
- `session_end {"session_id":"...","ended_at_unix":0}`
- `record_sample {"sample":{...}}`
- `record_event {"event":{...}}`

Lifecycle timestamps are optional and default to the current Unix time. Every
operation returns an `{ "ok": ... }` result. Storage failures are returned as
ordinary failed RPC results and increment the degraded-operation counter; they
do not fail the history module tick or the stream session.

Session sample `encode_latency_ms` is nullable. `null` means the producer has no
encoder timing measurement; a number is accepted only when a caller supplies a
measured value. The schema upgrade preserves historical sample rows and values,
while converting the old stream-session zero placeholder to unknown.

When `enabled` changes during active streams, the saved value is reported
immediately, but the effective recording decision remains in place until the
last active stream ends. New streams admitted during that interval use the
existing decision. The final recorded end and its retention pass commit before
the write connection closes. While disabled, a read-only connection serves
list, detail, active, and status requests. New sessions are not recorded,
explicit delete returns `recording_disabled`, and settings changes or idle
ticks do not run TTL or size retention. Re-enabling opens the writer and runs
interrupted-session recovery, per-session row trimming, and retention before
accepting new recorded sessions. A failed writer transition leaves history
readable and is retried on later ticks. If closing the disabled writer fails,
new recording admission closes after the admitted streams drain. Legacy SQLite
settings cleanup is deferred while read-only and resumes after re-enable;
cleanup failures do not block a saved recording policy. On restart the database schema is
opened before the central policy loads. Once that policy is known, unfinished
rows from a previous process receive an interrupted end marker before a
disabled writer closes. Per-session cap trimming and retention wait for an
enabled policy. The active list reflects only live sessions admitted by this
process.

History detail responses cap samples to the latest 32 and events to the latest
8. Timeline open captures the metadata and each retained row-ID range in one
read transaction. Follow each returned cursor to read samples or events in
admission order, at most 32 samples or 8 events per page and at most 16 KiB per
response. Appends after opening are excluded. If a row in the captured range is
deleted or pruned, a subsequent page returns `history_changed`; open a new
snapshot instead of combining incompatible data. Cursors are bound to the session,
kind and captured row range and are not credentials. Session listing is capped
at 25 items per page and active listing at 16.
Timeline pages also return `entry_ids`, decimal row identifiers aligned with
`entries`. They preserve full 64-bit integer precision and allow a client to
reuse rows across bounded live windows. An omitted `sample_limit` continues to
capture all retained samples; the limit affects samples only, not events.
Storage keeps the latest 7,200 samples and 2,000 events per session; inserts
and trimming commit together so failed trimming cannot leave an unbounded row.
Retention is applied by age and approximate live SQLite page bytes while the
writer is active. Both policies delete ended sessions only; active records are
retained even if the configured size limit cannot yet be met. An hourly sweep
runs when enabled and no sessions end. A size of zero disables its policy.

The pinned Vibeshine writer joins its draining queue and closes its write
database on disable. Its history reads and explicit deletion are unavailable
while disabled. Amaterasu keeps a separate read-only connection for history
inspection, while retaining the same paused retention and delete behavior.

## Integration status

The management HTTP routes and frontend editor expose these settings and the
history list, detail, active, status, and delete operations. The module now
loads and updates policy through the central configuration service. Stream
session lifecycle and metric integration remain separate work. Keep the three
configuration coverage IDs pending until those integrations are complete.
