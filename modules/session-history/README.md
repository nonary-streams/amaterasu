# Session history module

`amaterasu.session-history/1` owns the history SQLite database and processes
one request at a time. This gives database mutations a single ordered writer.
The database uses WAL mode. A trusted `database_path` module setting can
override its location; development profiles inject a path beneath the
package's private state directory. Production falls back to the current user's
application data directory under `Amaterasu/session-history/history.db`.
Configured database paths must be absolute file paths.

Initial settings default to `enabled: true`, `ttl_days: 0`, and
`db_size_limit_mb: 0`.

## RPC

Management calls are accepted only from `amaterasu.management`:

- `settings_get {}`
- `settings_update {"settings":{"enabled":true,"ttl_days":0,"db_size_limit_mb":0},"expected_revision":1}`
- `list_sessions {"limit":25,"offset":0}`
- `session_detail {"session_id":"..."}`
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

History detail responses cap samples to the latest 32 and events to the latest
8. Session listing is capped at 25 items per page and active listing at 16.
Retention is applied by age and approximate live SQLite page bytes. Both
policies delete ended sessions only; active records are retained even if the
configured size limit cannot yet be met. A size of zero disables its policy.

## Integration status

The management HTTP routes and frontend editor expose these settings and the
history list, detail, active, status, and delete operations. This first storage
slice still stores the three revisioned settings alongside the history
database. They are not yet synchronized with the central
`amaterasu.configuration/1` service, and stream-session lifecycle hooks remain
a separate integration step. The three configuration coverage IDs must stay
pending until both integrations are complete.
