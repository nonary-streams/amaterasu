# Session coordination

This original, in-memory library provides `amaterasu.session/1`. It coordinates
session records and resource ownership; it does not start capture, encode media,
negotiate protocols, or stream. Records and leases disappear when the module's
process exits. Resource identities are opaque, exact, case-sensitive strings;
callers must agree on canonical identities for a shared physical resource.

Requests use the runtime-authenticated `caller` as their owner. No payload field
can override ownership. Actions are:

```json
{"action":"create","key":"caller-idempotency-key"}
{"action":"activate","session_id":1}
{"action":"acquire","session_id":1,"resource":"capture/display/0","mode":"shared"}
{"action":"release","session_id":1,"resource":"capture/display/0"}
{"action":"stop","session_id":1}
{"action":"finish_stop","session_id":1}
{"action":"cancel","session_id":1}
{"action":"fail","session_id":1}
{"action":"snapshot","session_id":1}
{"action":"forget","session_id":1}
{"action":"list"}
{"action":"list","offset":16,"limit":16}
```

Creation enters `preparing`; activation enters `active`; stop enters `stopping`;
finish-stop enters `stopped` and releases the session's leases. Cancel and fail
perform stop and finish-stop together, including after partial startup. Repeated
creation with the same caller/key returns the retained record. Repeated valid
activation, stop, terminal cleanup, acquisition, and release are idempotent.
Stopped sessions cannot restart or acquire resources. A repeated terminal action
preserves the first outcome (`completed`, `cancelled`, or `failed`). A shared
resource may have multiple owners; exclusive ownership conflicts with all other
owners. Changing a session's lease mode requires release followed by acquisition.

Success is `{"ok":true,"revision":1,"sessions":[...]}`. Snapshot and mutation
records have `id`, `key`, `state`, nullable `outcome`, and a `leases` object mapping
resource identity to `shared` or `exclusive`. Forget returns an empty array. List
returns caller-only summaries with `lease_count` instead of the leases object.
List defaults to 16 records, accepts limits from 1 to 16, and returns a nullable
`next_offset`. Continue from that offset while the revision stays unchanged;
restart pagination if a concurrent mutation changes the revision.
Errors are ordinary payloads:
`{"ok":false,"revision":1,"code":"invalid_transition","message":"..."}`.
Absent, forgotten, and foreign session IDs all return `session_not_found`.

The coordinator retains at most 1024 sessions, 256 per caller, 1024 resource
identities, and 128 leases per session with a total of 1536 resource-identity bytes
per session. Keys are 1–128 bytes and resources are 1–256 bytes. These bounds and
list pagination keep replies below the runtime's 16 KiB payload limit, including
JSON escaping. Stopped records remain until the owner explicitly forgets them;
forgetting permits reuse of a key but never reuses its session ID within the
process lifetime. List summaries bound response size independently of lease
counts. There is no background expiry or automatic cleanup of disconnected
callers; callers must stop/cancel and forget their records.

Every committed state change increments the module-wide revision. When granted,
`amaterasu.session.changed/1` publishes `{"revision":N}` only. Events deliberately
omit caller identities, keys, resources, and outcomes. Events are advisory: failed
publication does not undo state, and consumers use snapshots/list to resynchronize.
Revisions and IDs restart with a fresh module process and are not durable epochs.

Unimplemented parity includes actual streaming startup/teardown, capture devices,
protocol integration, durable recovery, disconnect detection, and automatic
resource management across module/process lifetimes. Native modules remain trusted
code; broker grants are not a native-code sandbox.

`lookup {key}` returns the caller's existing session snapshot without creating a
session or changing its revision. Missing and foreign-owned sessions both return
`session_not_found`. Coordinators can use this after cancellation obscures the
result of an idempotent create operation.
