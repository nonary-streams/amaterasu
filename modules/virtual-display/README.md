# Virtual display adapter

`amaterasu_virtual_display` is an independently built native library providing
`amaterasu.virtual-display/1`. It implements the Sunshine Windows virtual display
control protocol with temporary owned leases. It does not install or modify a
driver. Settings default to
`{"enabled":false,"dd_activate_virtual_display":false,"dd_use_sunshine_virtual_display_driver":true}`.
The permanent-monitor setting `dd_virtual_display_permanent_count` has a range
of 0 through 4; the legacy name `dd_vdd_static_monitor_count` is also accepted,
and the canonical name takes precedence if both are supplied. Integral values
outside the range normalize to zero while remaining configured, matching
Vibeshine. Non-integer and null values are rejected. Status reports numeric zero
for an unset value and uses a separate configured flag to distinguish it from
explicit zero.
This setting does not change the eight-record limit for temporary leased
monitors.
`enabled` remains the module's service gate and is independent of the stable
configuration setting names. When it is false, create returns `disabled` without
touching the driver. When the module is enabled, create may manage a temporary
monitor regardless of `dd_activate_virtual_display`. That setting requests
stream-time topology activation; this service does not implement that behavior.
An explicit create only creates a temporary monitor and cannot activate it or make
it the only active display. Status reports the unsupported activation request.
If `dd_use_sunshine_virtual_display_driver` is false, create returns
`unsupported_driver`; this service has no alternative driver and does not silently
select Sunshine. Non-Windows create returns `unsupported_platform` when the module
is enabled and the Sunshine driver is selected.

The provider needs only its service registration grant; clients need its call
grant. There are no state, filesystem, or subprocess service dependencies.
`serde`/`serde_json` implement the bounded SDK JSON boundary, `sha2` derives stable
caller-specific IDs, `getrandom` supplies lease capabilities, and `windows-sys`
provides documented Win32 bindings. Native code remains trusted, not sandboxed.

## API

Requests reject unknown fields. Examples:

```json
{"action":"create","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09","width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu"}
{"action":"status"}
{"action":"status","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09"}
{"action":"remove","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09"}
```

Identity must be a nonzero canonical lowercase UUID. Resolution bounds are
320–7680 by 200–4320; refresh is 1000–1000000 millihertz. Name is 1–31 printable
ASCII characters. Physical size is currently fixed at 600 by 340 mm, with default
driver HDR luminance. At most eight records exist across callers.

Create returns `{"ok":true,"monitor":{...}}` when ownership is recorded, including
when a failed driver acknowledgement starts cleanup. Inspect `state` and `error`:
`creating`, `ready`, `removing`, or `cleanup_failed`. A `ready` value contains the
validated Windows monitor `device_path` and whether CCD reports the path active.
Readiness means the exact returned adapter/target has a published monitor identity;
it does not mean capture, encoding, streaming, or topology activation is ready.
Creation waits up to five seconds for that identity before cleanup starts.

Each caller sees/removes only its own records. Repeating create with identical
caller, identity, and specification returns the existing record without replaying
the native effect. Changing its specification returns `identity_conflict` until
removed. A SHA-256 derivation of caller and UUID yields the stable requested driver
ID; a fresh random lease/capability is used for each new lifecycle. Neither secret
is exposed. Missing remove is idempotent. A remove of another caller's UUID never
removes that caller's monitor. Status is a bounded snapshot, not persistent history;
it reports the module gate and both canonical setting names, marks activation as
unsupported through `activation_supported`, `activation_state`, and
`activation_error`, and identifies the driver as `unsupported` when the Sunshine
driver is not selected.

## Protocol provenance and safety

Behavior and ABI were inspected at Vibeshine commit
`aa292c4cb097f22beab19d6ebf0307367f9bf449`, whose pinned
`third-party/libvirtualdisplay` commit is
`a72ba1c50392bb8f4c0549022739732903fe44ba`, particularly
`src/driver/include/virtual_display/driver/control_protocol.h`. This adapter is an
original implementation; it does not copy or translate the reference implementation.

Supported protocol versions are exactly major 3, minor 6 or 7. Device interface is
`5f894d6c-3a69-48a2-86ef-e4c671932d63`; protocol namespace is
`a2864284-77fe-4336-a828-00feec89ebac`. IOCTLs use buffered FILE_DEVICE_UNKNOWN
control codes with these function numbers and byte layouts:

| Operation | Function | Input / output bytes |
| --- | --- | --- |
| Version | 0x900 | 0 / 24 |
| Owned temporary create | 0x90c | 128 / 56 |
| Exact lease/display remove | 0x902 | 32 / 0 |
| Lease feed | 0x903 | 32 / 0 |
| Lease query | 0x905 | 32 / 48 |
| State query | 0x908 | 0 / 1696 |

The byte codec explicitly encodes little-endian fields instead of relying on Rust
layout. Reply size, namespace, version, IDs and counts are checked before use.
Device and monitor paths require valid terminated UTF-16. Interface enumeration is
bounded and fails on ambiguous multiple driver interfaces. CCD enumeration allows
at most 256 paths/modes and does not retry a changing topology indefinitely.

IOCTLs use overlapped I/O with a 200 ms event timeout and exact-operation
cancellation. Cancellation is drained before releasing native buffers. A defective
driver that never completes cancellation can still block that drain; synchronous
CreateFile/device enumeration/CCD calls likewise have no module-level interrupt.
The isolated host worker watchdog is the final recovery boundary for these hangs.
No background thread can outlive this DLL. These limits are not a real-time promise.

A ten-second lease is fed once per second while healthy. One monitor is maintained
per tick, with 200 ms polling. Ambiguous creation is never replayed: it enters exact
lease/display cleanup. Cleanup makes at most three native removal attempts and keeps
its record until state and pending-departure queries confirm removal. Failed cleanup
stops lease feeding. Drop makes one best-effort exact removal per owned record;
there is no remove-all command. A newly admitted create whose completion is cancelled or cannot be delivered enters
exact cleanup and stops feeding; failed completion of an idempotent retry leaves
the previously admitted monitor intact. Crash/cancellation after create can leave a
temporary monitor until cleanup or driver lease expiry; driver failure can delay
actual OS departure.

## Remaining release gates

Real Windows hardware/driver qualification remains required; tests use a fake backend
and byte fixtures, and cross-compilation does not prove driver interoperability.
Durable lease capability storage/reclaim and restart recovery are unimplemented.
The driver ID remains stable on a later create, but this is not durable ownership:
a crash can require waiting for the previous lease to expire. Driver identity
retention/cleanup policy, persistent monitor manifests, adapter selection, HDR/ICC,
DPI controls, and the incompatible SudoVDA protocol are not supported. There is no
automatic session/launch coordination or Linux DRM virtual display backend yet.

Tests exercise setting defaults and separation, activation diagnostics and driver
selection, caller isolation, stable identity/idempotency, readiness/feeding,
ambiguous creation, bounded exact cleanup, departure confirmation, and malformed protocol
lengths/counts/UTF-16. Persistent global configuration projection and per-client
behavior remain pending; the defaults and module settings payload do not provide
that configuration plumbing. The `dd_activate_virtual_display` stream-time
topology behavior also remains pending. Permanent-monitor creation is unsupported:
status reports the configured count and `permanent_monitor_runtime_supported:false`,
but this module does not ask the driver to create permanent monitors. Product
settings persistence and migration, and management edit authority remain pending.
Windows driver qualification for permanent monitors has not been run. Run `cargo test -p
amaterasu-virtual-display` and cross-check with `cargo check -p
amaterasu-virtual-display --target x86_64-pc-windows-msvc`.
