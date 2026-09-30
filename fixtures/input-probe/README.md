# Synthetic input probe

`amaterasu-input-probe` is a managed in-memory provider for signed stream input
integration. It provides `amaterasu.input/1` and `test.input-probe/1`. It records
validated requests and never invokes a native input API, so success proves module
orchestration and protocol handling rather than an operating-system input effect.

The startup setting is `{"enabled":false}`. Acquisition defaults off and must be
enabled explicitly in a test profile.

The input service accepts:

```json
{"action":"acquire","operation_id":"session.input","ttl_ms":5000}
{"action":"lookup","operation_id":"session.input"}
{"action":"renew","lease":"<returned token>","ttl_ms":5000}
{"action":"batch","lease":"<returned token>","events":[{"kind":"key","key":{"platform":"moonlight_vk","code":65},"state":"down"}]}
{"action":"release","lease":"<returned token>"}
```

Leases belong to the immutable runtime caller. TTL is 1–30 seconds and defaults
to five seconds. An acquire operation ID is optional. When supplied, the exact
same caller, ID, requested TTL and requested input policy return the existing
result without extending the lease. A changed TTL or policy conflicts. Lookup is owner scoped and retains released,
expired and cancelled tombstones. Missing operation IDs are not recoverable.
There are at most 128 lifetime leases and 128 retained operation records.

Acquisition also accepts the nullable scoped keyboard, mouse, controller, Back
timeout, gamepad, pen/touch, scrolling, scancode, Right Alt, keybinding and repeat fields from
the input lease contract. The fixture preserves explicit `false` and zero values.
Gamepad names must be canonical choices recognized by the input contract on
the current target; recognition does not imply native availability. Repeat
delay accepts whole milliseconds from zero through 2147483647; repeat
frequency accepts finite nonnegative numbers. It records the request without
resolving global defaults or simulating native input effects. The typed
`keybindings` request preserves `null`, explicit `[]`, pair order, and duplicates.
It accepts an even flat list of at most 512 exact integer bytes in 0 through 255;
no global list, built-in mapping, or native delivery is emulated. Omitted and null
requests both record inheritance, while an explicit empty list remains distinct
for active and terminal operation replay.

Batches contain 1–64 events and are validated atomically before recording. The
fixture accepts Moonlight virtual-key codes 1–255 with down, up and repeat state;
the five existing mouse buttons; relative deltas from -32767 to 32767; and
horizontal or vertical integer scroll steps from -100 to 100 excluding zero.
Repeat requires a held key. Duplicate presses and unowned releases fail. Absolute
events record unsigned normalized coordinates from 0 to 65535. The fixture does
not invent a capture viewport; the session must map authoritative geometry before
sending these events. No native pointer is moved.
At most 128 keys and buttons may be held.

`{"action":"status"}` on `test.input-probe/1` returns:

```json
{
  "ok": true,
  "fixture": true,
  "enabled": true,
  "active": true,
  "acquired": 1,
  "renewed": 2,
  "released": 0,
  "expired": 0,
  "events": [],
  "events_truncated": false,
  "held_keys": 0,
  "held_buttons": 0,
  "requested_policy": {
    "back_button_timeout": null,
    "controller": null,
    "gamepad": null,
    "ds4_back_as_touchpad_click": null,
    "native_pen_touch": null,
    "keyboard": null,
    "mouse": null,
    "high_resolution_scrolling": false,
    "always_send_scancodes": false,
    "key_rightalt_to_key_win": true,
    "keybindings": [],
    "key_repeat_delay": 0,
    "key_repeat_frequency": 0.0
  }
}
```

The last 256 canonical events are retained in order. `events_truncated` remains
true after the first eviction. Metrics expose counts and events but never the
lease token, operation ID or owner. `requested_policy` is null without an active
lease. Lost successful request completion cancels
the caller's active lease so an ambiguous mutation cannot retain held input.
