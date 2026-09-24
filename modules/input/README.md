# Keyboard and mouse input module

`amaterasu-input` provides `amaterasu.input/1` as an independently loadable native
`cdylib` and an `rlib`. The provider needs a service grant; callers need a call
grant. It uses `amaterasu.configuration/1` for durable product settings. Native modules are trusted code;
API grants are not a native-code sandbox.

Startup settings:

```json
{"allow_injection":false,"uinput_path":"/dev/uinput"}
```

Injection defaults off. The Linux path is an administrator setting, never a request
field. It must be an absolute, non-symlink character-device path and is opened
only when an enabled acquire reaches its active-request check. No devices are
created merely by loading the module. Windows ignores `uinput_path`. macOS and
other platforms report unsupported native injection instead of simulated success.

## Durable input policy

The input module loads the caller-scoped product document `product/amaterasu.input`
after startup. Version 6 stores `keyboard`, `mouse`,
`high_resolution_scrolling`, `always_send_scancodes`, and
`key_rightalt_to_key_win`, `key_repeat_delay`, and
`key_repeat_frequency`, and `back_button_timeout`; an absent document enables keyboard, mouse,
high-resolution scrolling, and scancode mode, leaves right-Alt mapping disabled,
uses a 500 ms initial key-repeat delay at 24.9 Hz, and disables the Back timeout
with `-1`. Versions 1 through 5
migrate in memory with the new repeat defaults. Version 1 documents migrate in
memory with high-resolution
scrolling and scancode mode enabled, preserving saved keyboard and mouse values.
Version 2 documents preserve all three saved values and default scancode mode to
enabled. Version 3 preserves all four earlier values and defaults right-Alt
mapping to disabled. The next update writes the complete v6 document. Delay is
an integer from 0 through 2,147,483,647 milliseconds; zero disables repeat.
Frequency is a finite nonnegative decimal in Hz. Zero keeps Vibeshine's 24.9 Hz
default. Back timeout is a signed 32-bit millisecond value; any negative value
disables the shortcut. The module needs calls to configuration and state
and dependencies on both services; the configuration service uses the caller's
delegated authority for its durable state read. Management cannot select another
product namespace.

Administrators use the fixed management routes `/v1/input/settings_get` and
`/v1/input/settings_update`, protected by `configuration.read` and
`configuration.write`. An update body contains both keyboard and mouse fields,
`expected_revision`, and may contain `high_resolution_scrolling`,
`always_send_scancodes`, and `key_rightalt_to_key_win`. Older updates that omit
newer fields preserve their current durable values. The update may also include
`key_repeat_delay`, `key_repeat_frequency`, and `back_button_timeout`. Product settings
actions accept only the exact runtime caller `amaterasu.management`.

```json
{"keyboard":false,"mouse":true,"high_resolution_scrolling":true,"always_send_scancodes":true,"key_rightalt_to_key_win":false,"key_repeat_delay":500,"key_repeat_frequency":24.9,"back_button_timeout":-1,"expected_revision":4}
```

The policy gates event delivery after it is loaded. Disabled keys are suppressed;
disabled pointer, button, and scroll events are suppressed. Suppressed events
still count as accepted in successful batches so stream batch acknowledgements
remain aligned with the submitted event count. Existing keys/buttons in a newly
disabled category are released in reverse press order before the settings update
is acknowledged. A failed native release leaves the new policy persisted and
gated for new events, returns `ok:false` with `error.code:"cleanup_pending"`,
`persisted:true`, `applied:false`, and `cleanup_required:true`, and retries
cleanup. Settings reads report the durable values and whether cleanup is
complete. Configuration calls run asynchronously; while a policy write is
pending, later mutating input requests are held in a bounded FIFO queue. Status
and lease lookup remain available, and unrelated runtime services continue to
be polled. Settings reads do not block on the startup fetch; they return
`settings_unavailable` until the stored document has loaded rather than presenting
bootstrap defaults as persisted settings.

The live global policy includes `keyboard`, `mouse`,
`high_resolution_scrolling`, `always_send_scancodes`,
`key_rightalt_to_key_win`, `key_repeat_delay`,
`key_repeat_frequency`, and `back_button_timeout`. When high-resolution
scrolling is disabled, the input controller accumulates signed wheel units
separately per axis and emits legacy 120-unit detents. Fractional remainders are
discarded on mode changes, mouse disable, and lease end. The default preserves
raw high-resolution units.

On Windows, `always_send_scancodes` affects non-normalized Moonlight keyboard
packets only. When enabled, the backend maps their virtual key through the
current Windows keyboard layout and injects a scan code; Windows, Pause, and
unmapped keys fall back to virtual-key injection. When disabled, non-normalized
packets use virtual-key injection. Normalized packets retain the existing fixed
US physical-key mapping in either mode. The backend remembers the representation
selected on key-down so repeats, releases, and lease cleanup remain paired if
the setting changes while a key is held. Linux intentionally ignores this
setting and maps either packet form through the existing fixed US-to-evdev table;
unmapped raw keys are rejected during batch validation.

Linux creates per-lease uinput gamepads when its startup probe succeeds. It maps
controller number and active mask across 16 slots, projects assigned buttons,
triggers, and signed axes, removes inactive slots, and retries failed
timeout-driven state writes while retaining slot ownership. The global
`back_button_timeout` defaults to `-1`; nonnegative values schedule a Back
deadline per slot. On expiry, the module releases Back, pulses Guide for 100
milliseconds after successful delivery, and suppresses Back until the client
reports it released. Slot removal and lease teardown cancel pending timers.
Windows currently reports controller input unsupported. Per-app and per-client
`back_button_timeout` overrides are not yet resolved, and platform-qualified
controller parity remains pending.

When `key_rightalt_to_key_win` is enabled, Moonlight VK_RMENU (`0xA5`) maps to
VK_LWIN (`0x5B`) before native injection. This applies to normalized and raw
Moonlight packets on Windows and Linux. The backend records the native key chosen
at key-down so a setting change cannot send key-up for a different key. Linux
maps VK_LWIN through its existing table to `KEY_LEFTMETA`; Linux runtime behavior
has not been exercised on this Windows host.

Key repeat belongs to the active input lease. A new key-down replaces that
lease's current repeat key and starts the configured initial delay; the matching
key-up, lease end, keyboard disable, native failure, or a repeat-policy change
cancels it. Duplicate GameStream key-down packets are ignored, matching
Vibeshine's client behavior. Each module tick emits at most one repeat and
advances to the next scheduled deadline without replaying missed intervals.
The input worker ticks every 10–20 ms, so actual cadence has that scheduler
jitter. Captured synthetic Shift/Ctrl/Alt modifiers are pulsed only when no
equivalent real modifier is held; their markers are validated across batches and
remain tracked for lease cleanup on native failure. Linux emits the same
repeat-event form through uinput, but runtime typing/repeat behavior has not
been qualified on this Windows development host.

## JSON API

```json
{"action":"status"}
{"action":"acquire","operation_id":"session.input.7","ttl_ms":5000}
{"action":"lookup","operation_id":"session.input.7"}
{"action":"batch","lease":"<returned token>","events":[{"kind":"key","key":{"platform":"windows_scan","code":30,"extended":false},"state":"down"},{"kind":"relative","dx":10,"dy":-5},{"kind":"button","button":"left","pressed":true}]}
{"action":"renew","lease":"<returned token>","ttl_ms":5000}
{"action":"release","lease":"<returned token>"}
```

One random 128-bit lease token belongs to the authenticated immediate runtime
caller. Requests cannot override that identity. Another caller or an old token
cannot inject, renew, or release it. Acquire succeeds only when no lease or
unresolved cleanup exists. Lease duration is 1–30 seconds, default 5 seconds;
renew before expiry. Batches do not extend the lease automatically. Status shows
backend capabilities and lease/cleanup information, but reveals the token only
to its owner. Backend support reports compiled capability, not OS permissions or
proof that an application/compositor received input.

`operation_id` is optional for compatibility. When present, it is a private
1–128 character identifier scoped to the authenticated caller. Retrying the exact
same acquire returns the same bearer without extending its expiration; changing
the requested TTL returns `operation_conflict`. `lookup` returns that caller's
active acquire, including the bearer, or a retained terminal tombstone with
`state:"released"`, a null bearer, and reason `released`, `expired`, `cancelled`,
or `failed`. Unknown and foreign operations return `not_found`. Up to 64 terminal
operations are retained for five minutes; new recoverable acquires fail visibly
while that bound is full. Acquires without an operation ID are not lookup
recoverable. A non-idempotent batch must never be replayed after an ambiguous
broker result; its owner settles the broker call, releases or looks up the lease,
and ends the input path.

Active acquire and lookup responses include the backend's `absolute_pointer`
capability. Callers must require it before sending absolute events; terminal
tombstones omit capabilities.

Batches contain 1–64 events. The entire batch is validated against backend
capabilities and a simulated pressed state before its first native event.
At most 128 keys/buttons may be held. Supported events:

- `key`: Windows requires `windows_scan` with a set-1 scan code from 1–127 and
  optional `extended`; Linux requires `linux_evdev` with a keyboard code from
  1–248, excluding `KEY_UNKNOWN` (240). These are native encoding ranges, not a
  portable key map; clients must choose a defined code for the target platform.
  Trusted protocol adapters may instead use `moonlight_vk` with a normalized,
  allowlisted Win32 virtual-key code. The module's fixed US physical mapping
  produces the corresponding Windows set-1/extended or Linux evdev key. Generic
  and left Shift/Ctrl/Alt aliases have one pressed identity. A lease cannot mix
  native and Moonlight key encodings, preventing two names from owning one native
  key independently.
  `moonlight_vk.normalized` is optional and defaults to `true` for older clients.
  Protocol adapters set it to `false` for a non-normalized virtual-key packet.
  Windows then applies `always_send_scancodes`; Linux uses its fixed mapping and
  rejects raw virtual keys without an evdev mapping.
  `state` is `down`, `up`, or `repeat`. Repeat requires a held key. Duplicate
  presses and releases of unowned keys are rejected.
- `button`: `left`, `right`, `middle`, `back`, or `forward`, with `pressed` true or
  false. Duplicate presses and releases of unowned buttons are rejected.
- `relative`: `dx` and `dy` from -32767 to 32767. OS pointer acceleration can
  affect the resulting movement.
- `absolute`: Windows-only normalized unsigned coordinates `x` and `y` from
  0 to 65535 over the **entire virtual desktop**. This does not infer a captured
  monitor or viewport. Linux absolute mapping is explicitly unsupported.
- `scroll`: `axis` is `vertical` or `horizontal`; `steps` is a nonzero integer
  from -100 to 100. One step is 120 Windows wheel units or one evdev wheel detent.
- `scroll_units`: `axis` is `vertical` or `horizontal`; `amount` is a nonzero
  signed 16-bit wheel-unit delta. This preserves Moonlight's raw fractional
  scrolling. When the global high-resolution option is disabled, the controller
  accumulates each axis and emits `scroll` detents instead.

Linux key example:

```json
{"kind":"key","key":{"platform":"linux_evdev","code":30},"state":"down"}
```

On Linux, high-resolution wheel injection sends both `REL_WHEEL_HI_RES` /
`REL_HWHEEL_HI_RES` units and compatible legacy detents as each axis reaches 120
units. Linux event encoding is unit-tested but runtime delivery still needs
verification on Linux hardware.

Success responses contain `ok:true` and the lease/accepted event count or release
result. Errors contain `ok:false`, `error.code`, `error.message`, and
`cleanup_required`. Injection errors also report the fully accepted event prefix
and `partial_event_possible:true`. Batches are not atomic OS transactions: pointer
movement or an input prefix cannot be undone, and even a failing event may have
partially reached the OS. A disabled-high-resolution raw wheel packet can
produce several bounded legacy detent events. If native injection fails after
earlier detents were delivered, that packet has a partially applied prefix; its
lease cannot accept another batch. Successful cleanup ends the lease and clears
fractional remainder; failed cleanup leaves the lease marked `cleanup_required`
while releases are retried. Clients must not replay the wheel packet under that
lease. Reconcile lease status before retrying an ambiguous request. Reuse the
runtime operation ID only for the same logical request.

## Pressed-state ownership and cleanup

The controller tracks downs before calling the backend, because a failure may
occur after the OS consumed a down but before acknowledgement. It forgets a
release only after the backend accepts it. Chords release in reverse press order.
A runtime provider-only `CheckRequest` precedes acquisition and every native batch
event. Cancellation, request expiry during injection, injection failure, explicit
release, lease expiry, and graceful module unload all attempt owned releases.
If completion loses a race with cancellation, the caller's lease is also cleaned
up. Cleanup itself remains authorized by the original lease.

Failed releases remain tracked, mark `cleanup_required`, and prevent renewal,
new input and takeover by another caller. The owning caller can retry release;
the module also retries cleanup at most every 100 ms while ticking. Lease expiry
is checked while the module runs, including between batch events. Shutdown makes
one final release attempt. Linux additionally destroys its owned virtual devices.
Windows cannot guarantee key-up delivery when the OS rejects SendInput, or after
a hard process crash; there is no separate crash-surviving cleanup watcher yet.
The lease coordinates this module's callers, not physical keyboards/mice or other
input-injecting processes.

## Native backends

Windows uses `SendInput`, scan-code keyboard flags, five mouse buttons, relative
movement, virtual-desktop absolute movement and vertical/horizontal wheel events.
Each native call submits one INPUT record, with checked acceptance. It does not
bypass UIPI, the secure desktop, session boundaries or application restrictions.
Text/Unicode composition, E1/Pause sequences, layout-aware key translation,
AltGr/platform remapping and automatic text repeat policy remain adapter work.

Linux creates separate virtual keyboard and relative-pointer devices through
uinput. It enables the relevant EV_KEY/EV_REL capabilities, configures devices
with `UI_DEV_SETUP`, and emits frames terminated by `SYN_REPORT`. Descriptor
creation is lazy and transactional: failure creating the second device destroys
the first. Nonblocking write errors/partial frames propagate into cleanup; there
is no placeholder acceptance on failed writes. Devices are destroyed on lease
end and descriptor teardown.

The uinput ioctl base/numbers and event codes were checked against Linux v6.12
UAPI headers. `libc::_IO/_IOW` supply architecture-specific encoding, and libc
supplies `uinput_setup`, `input_id`, and `input_event` layouts. Linux x86-64 builds
assert the 92-byte setup, 24-byte event and ioctl encodings at compile time.
Safety comments describe initialized buffers and pointer lifetimes at each ioctl
and write. There are no handwritten C structs or borrowed device pointers.

The kernel must support `UI_DEV_SETUP`, and the service account must be permitted
to open the configured uinput device. No permissions/udev rules are changed by
this module. Device creation does not acknowledge compositor readiness; udev and
libinput discovery are asynchronous, and delivery during device activation needs
native acceptance testing. Seat selection, captured-monitor coordinate mapping,
Wayland absolute-pointer integration, touch/pen, controllers/rumble, and
crash-surviving Windows cleanup remain unimplemented.

## Validation and reference

Tests use a fake backend for ownership, stale tokens, batch admission, chord
release order, cancellation, partial failure, expiry, retained failed releases,
retry and shutdown. Windows encoding tests inspect INPUT records without calling
SendInput. Windows and Linux native sources cross-compile from macOS. **No test
injects input or creates a uinput device on the developer desktop.** Native
Windows/KDE Wayland delivery and failure behavior remain hardware gates.

Vibeshine commit `aa292c4cb097f22beab19d6ebf0307367f9bf449` was a behavioral
reference, including its keyboard/mouse interfaces and history concerning
captured-monitor absolute motion and release ownership. No implementation source
was copied or translated; captured-monitor mapping is an explicit remaining gap.

References: [Windows SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput),
[MOUSEINPUT coordinates](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput),
[Linux uinput guide](https://kernel.org/doc/html/latest/input/uinput.html),
[Linux v6.12 uinput UAPI](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/uinput.h),
[Linux v6.12 event codes](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/input-event-codes.h).
