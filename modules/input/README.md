# Keyboard, mouse, and controller input module

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
after startup. Version 11 stores `keyboard`, `mouse`, `controller`,
`high_resolution_scrolling`, `always_send_scancodes`, and
`key_rightalt_to_key_win`, `keybindings`, `key_repeat_delay`, and
`key_repeat_frequency`, `back_button_timeout`, and the global `gamepad` profile;
it also stores `ds4_back_as_touchpad_click` and `native_pen_touch`. An absent
document enables keyboard, mouse, and controller input,
high-resolution scrolling, and scancode mode, leaves right-Alt mapping disabled,
uses a 500 ms initial key-repeat delay at 24.9 Hz, and disables the Back timeout
with `-1`, and selects `auto` for gamepads. Versions 1 through 7 migrate in
memory with `auto` for the profile and their prior defaults. Versions 1 through 5
migrate in memory with the new repeat defaults. Version 1 documents migrate in
memory with high-resolution
scrolling and scancode mode enabled, preserving saved keyboard and mouse values.
Version 2 documents preserve all three saved values and default scancode mode to
enabled. Version 3 preserves all four earlier values and defaults right-Alt
mapping to disabled. Versions 1 through 7 default controller input to enabled.
Versions 1 through 8 default the DS4 Back mapping to enabled, and versions 1
through 9 default native pen/touch to enabled. Missing native pen/touch in a
version 10 or 11 document also defaults to enabled. Versions 1 through 10 add
the default flat keybinding list `[16,160,17,162,18,164]` while retaining all saved
policy values. A missing v11 keybinding field uses that same default. The next
update writes the complete v11 document. Delay is
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
`always_send_scancodes`, `key_rightalt_to_key_win`, and `keybindings`. Older updates
that omit newer fields preserve their current durable values. The update may also include
`key_repeat_delay`, `key_repeat_frequency`, `back_button_timeout`, `gamepad`,
`ds4_back_as_touchpad_click`, `native_pen_touch`, and the optional `controller`
boolean. Omitted controller and native pen/touch fields preserve their durable
values for older clients. The signed `/v1/input/status` read route reports
controller policy separately from backend capability. Controller policy is
currently global; app and paired-client controller overrides remain
unimplemented.
Windows recognizes `auto`, `x360`, `ds4`, `vhf`, `vhf_xbox`, `vhf_xbox_one`,
`vhf_ds4`, `vhf_ds5`, and `vhf_switch`. Linux recognizes `auto`, `xone`, `ds4`,
`ds5`, and `switch`. These names follow Vibeshine's pinned option lists at
`457237db7e57f9c4be21c35bb07e28d1c100eb7d`. Unsupported-but-recognized profiles
remain valid settings so deployments can retain an intended choice; a lease
reports them unavailable, rejects controller snapshots, and never silently
selects another profile. The next update writes the complete v11 document.
Product settings actions accept only the exact runtime caller
`amaterasu.management`.

```json
{"keyboard":false,"mouse":true,"controller":true,"high_resolution_scrolling":true,"always_send_scancodes":true,"key_rightalt_to_key_win":false,"key_repeat_delay":500,"key_repeat_frequency":24.9,"back_button_timeout":-1,"gamepad":"auto","native_pen_touch":true,"expected_revision":4}
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

The live global policy includes `keyboard`, `mouse`, `controller`,
`high_resolution_scrolling`, `always_send_scancodes`,
`key_rightalt_to_key_win`, `keybindings`, `key_repeat_delay`,
`key_repeat_frequency`, `back_button_timeout`, `gamepad`,
`ds4_back_as_touchpad_click`, and `native_pen_touch`. Native pen/touch defaults
to enabled. The input module stores and snapshots this setting, which gates the
typed native pen/touch event path independently from backend capability.
Each active lease keeps its resolved value when global settings change, so an
update applies to future leases. Release, expiry, failure, and shutdown cancel
lease-owned contacts; failed cancellations remain owned for cleanup retry.
Setting
`controller:false` suppresses controller snapshots and removes active owned
virtual gamepad slots while leaving keyboard and mouse delivery available. If
slot removal fails, controller snapshots remain suppressed and slot cleanup is
retried at 100 ms intervals; it does not block keyboard or mouse input. Re-enabling
controller input does not restore old slot state: a new controller snapshot must
arrive first. When high-resolution
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

With `gamepad:"auto"`, Windows probes a real ViGEmClient/ViGEmBus connection
and selects `x360` only after that connection succeeds. Linux probes each
native Auto route and reports controller support when at least one route can
create and destroy its device. Explicit `x360`, Windows `ds4`, and Linux
`xone`, `ds4`, `ds5`, and `switch` select their corresponding platform probes;
other recognized explicit profiles return `controller_input:false` with a
status reason, leave keyboard and mouse usable, and never fall back to `auto`.
Status reports the requested `gamepad`, `supported_profiles`, and the effective
profile as `effective_gamepad_profile`. Linux Auto reports `auto` as its
configured selector, not as one controller's selected route; `controller_input`
reports whether at least one native Auto route passed its probe. Other profiles
report null when their probe fails. `supported_profiles` lists implemented
profiles, not a promise that a host can open the required device node or driver.
An active lease retains the requested and effective profiles from acquisition.

Linux creates per-lease Xone and Switch uinput gamepads when their startup
probes succeed, and maps controller number and active mask across 16 slots. It
projects profile-specific buttons, triggers, and signed axes, removes inactive
slots, and retries failed timeout-driven state writes while retaining slot
ownership. The global
`back_button_timeout` defaults to `-1`; nonnegative values schedule a Back
deadline per slot. On expiry, the module releases Back, pulses Guide for 100
milliseconds after successful delivery, and suppresses Back until the client
reports it released. Slot removal and lease teardown cancel pending timers.
Each input lease accepts an optional signed `back_button_timeout` in its acquire
request. NVHTTP resolves the certificate-bound client value before the
application value; when both are absent, input uses the durable global policy.

When the Linux companion backend is selected, its host-owned
`input-companion-policy.json` must also authorize `gamepad:true` for the exact
verified component-set digest. Missing `gamepad` fields retain the old
keyboard/mouse policy behavior and grant no gamepad capability. The module's
`controller` setting remains a separate event gate. The companion creates a
uinput gamepad only when an authorized controller slot first becomes active;
inactive slots, worker disconnect, permit revocation, and service teardown
neutralize and destroy companion-owned devices. This policy document is
digest-bound by the host but is not itself a cryptographically signed profile
field.

Input captures the effective value when the lease starts, so a global settings
change during a stream affects the next lease and does not change the current
stream's controller behavior.

After a failed Linux gamepad report write, the module destroys the possibly
partially updated uinput device before recreating its slot and sending the
current state. A failed destroy retains the old device for cleanup retry; a
failed new allocation leaves the slot without a device until the next tick.
This recovery has been checked with fake devices; Linux target compilation and
native delivery still need qualification on a Linux build host with uinput.

Linux `ds4` creates a USB-identity DualShock 4 over `/dev/uhid` (Sony VID/PID
054c:05c4). It publishes the DS4 input, output, and feature-report descriptor,
answers calibration, pairing, control, and firmware feature queries, and sends
64-byte reports for buttons, d-pad, triggers, sticks, battery, two touch
contacts, accelerometer, and gyroscope. Motion samples use m/s² for
acceleration and degrees per second for gyroscope; touch coordinates and
pressure are normalized to `[0,1]` and projected onto the DS4's 1920×942
surface. Each virtual device receives a random local-unicast identity that
remains stable if a failed report requires device replacement. Back keeps its
Share mapping and also sets the touchpad-click bit
when `ds4_back_as_touchpad_click` is enabled. Failed input writes retire and
recreate the UHID device before replay; failed destruction keeps the descriptor
owned for cleanup retry. Valid rumble and lightbar output reports are routed
through the active input lease. Synthetic descriptor, report, feature-query,
feedback, and lifecycle tests cover this path; no physical-controller or
game-delivery result is claimed.

Linux `ds5` prefers the optional `/dev/vibeshine-ds5` native USB gadget and
falls back to `/dev/uhid` when the gadget cannot be acquired. Both paths expose
one USB-identity DualSense (Sony VID/PID 054c:0ce6, version 0x8111); they are
never created together for one controller. A generation-tagged process
allocator reserves the kernel's four exclusive UAPI slots independently of
client controller numbers and MAC values, skipping slots already owned by
another process. The native gadget also supplies a four-channel USB-audio event
stream. When the stream negotiates HAPTICS_PCM and
the active controller advertises its per-controller HAPTICS_PCM capability,
the host selects the two actuator channels and groups them into bounded
240-frame (5 ms at 48 kHz) PCM messages with a wrapping stream sequence.
Misaligned source records are ignored and partial packets are dropped after an
input gap. The UHID fallback has no PCM source. Both paths publish the USB
report descriptor, answer calibration, pairing, firmware, and supported control
feature requests, and send 64-byte input reports at 100 Hz after startup.
Reports include buttons,
d-pad, triggers, sticks, battery state, two touch contacts, accelerometer
samples in m/s², and gyroscope samples in degrees per second. Touch coordinates
are normalized by the protocol and projected onto the DualSense 1920×1080
surface; normalized touch pressure above 0.5 marks a contact active. By default
each virtual device gets a random local-unicast
identity that remains stable across device recovery. Disabling
`ds5_inputtino_randomize_mac` uses `02:00:00:00:00:<slot>` for compatibility.
Failed input writes stop the event worker, destroy the potentially inconsistent
device, and recreate the same slot and identity before the next report. Failed
destruction retains the native descriptor for cleanup retry. Valid rumble,
lightbar, and adaptive-trigger output reports are routed through the active
input lease. Synthetic UAPI, packetization, USB descriptor, feature-query,
identity, input-report, recovery, and release tests cover these paths; they do
not establish driver availability, physical-device behavior, or game delivery.

Linux `switch` creates a USB-bus uinput device named
`Sunshine Nintendo (virtual) pad` with VID/PID `057e:2009` and version `0x8111`.
It maps client A/B/X/Y buttons to Nintendo's east/south/north/west face layout,
Home to Mode, Back to Select, capture to Z, and L2/R2 to digital ZL/ZR keys.
D-pad directions use hat
axes with up and left winning when opposite directions arrive together; stick
Y axes are inverted, and stick calibration uses fuzz 250 and flat 500. Touch,
motion, and battery reports are unsupported for this profile. It does not
advertise force feedback because the input service has no callback to deliver
rumble requests to the client. Synthetic identity, report-mapping, probe,
recovery, and release tests cover this path; hardware and game delivery remain
unqualified.

For Linux `auto`, Xbox arrivals use `xone`; PlayStation arrivals use `ds5`;
Nintendo arrivals use `switch`; and unknown devices with enabled motion or
touchpad preferences and matching capabilities use `ds5`. Unknown arrivals
without those capabilities use `xone`. Auto probes the xone, DS5, and Switch
backends independently, reports the configured selector as
`effective_gamepad_profile:"auto"`, and accepts controller input when at least
one route is available. Each arrival keeps its selected route: if that profile
cannot be created, the event returns `controller_unavailable` and does not fall
back to another profile. `controller_input` reports aggregate Auto viability; it
does not promise that every controller route is available.

Windows uses ViGEm's Xbox 360 profile when a
compatible ViGEmBus driver is installed and a vetted `ViGEmClient.dll` is
deployed in an allowed load location. The lease-start probe connects to the bus
independently of keyboard and mouse support; a missing DLL or driver leaves those
input paths available, reports `controller_input:false`, and rejects controller
snapshots with `controller_unavailable`. Windows creates
targets only for active slots, projects complete button, trigger, and signed-axis
snapshots, and removes targets when their active-mask bit clears or the lease
ends. Timer-driven update failures stay dirty and retry on module ticks; failed
detach remains owned for release retry. The global and per-lease Back timeout
state machine is shared across Linux and Windows. Windows can expose a DualShock
4 target when ViGEmClient provides its DS4 allocation and report exports. That
report path maps d-pad, standard buttons, triggers, and sticks; it does not
provide touch coordinates, motion, rumble, or controller feedback.
`ds4_back_as_touchpad_click` defaults to enabled and adds the Back button to the
DS4 touchpad-click control while preserving its Share mapping. Client touchpad
clicks map to that DS4 control where the selected native DS4 profile supports
them. The durable setting
and sparse acquire override are accepted by the input module. Signed management
and paired-client editors persist explicit false and inheritance; NVHTTP resolves
certificate-bound client policy before application policy and freezes the choice
for launch. Signed host restart and synthetic input-probe tests qualify that
projection. Windows DS4 touchpad-click and game delivery remain unqualified.
Windows does not select another profile when ViGEm is missing. Linux provides
its Xbox-like uinput backend under `xone`, native UHID implementations for `ds4`
and `ds5`, and the Nintendo Switch uinput profile; it does not link or call
Inputtino. VHF profiles and VHF/UMDF device creation remain unimplemented. Linux
virtual-device runtime behavior has not been exercised with a physical
controller or game, and Windows virtual-device delivery has not been qualified
against an installed ViGEmBus driver, so platform-qualified controller parity
remains pending.

If an active ViGEm target rejects a report, input detaches that target before
creating a replacement and submitting the current state. A failed detach keeps
the old target owned for later cleanup; a failed replacement remains retryable
on module ticks. This recovers a stale target within the existing client
connection. If target removal fails, input treats the shared client connection
as unusable and disconnects it, removing all attached targets, then retries
opening a new client once per second. Each active slot keeps its latest state
and Back timer, then recreates its target and resends that state after the new
connection succeeds. Lease release during reconnection cancels the retry.
Real ViGEmBus removal and restoration still need hardware qualification.

When `key_rightalt_to_key_win` is enabled, Moonlight VK_RMENU (`0xA5`) maps to
VK_LWIN (`0x5B`) before native injection when the selected `keybindings` list has
no custom source `0xA5` pair. A custom pair takes priority over this alias. This
applies to normalized and raw Moonlight packets on Windows and Linux. The backend records the native key chosen
at key-down so a setting change cannot send key-up for a different key. Linux
maps VK_LWIN through its existing table to `KEY_LEFTMETA`. Bounded grabbed Linux
keyboard tests inspect mapped native targets; desktop and game delivery remain
unqualified.

Key repeat belongs to the active input lease. A new key-down replaces that
lease's current repeat key and starts the configured initial delay; the matching
key-up, lease end, keyboard disable, native failure, or a repeat-policy change
cancels it. Duplicate GameStream key-down packets are ignored, matching
Vibeshine's client behavior. Each module tick emits at most one repeat and
advances to the next scheduled deadline without replaying missed intervals.
The input worker ticks every 10–20 ms, so actual cadence has that scheduler
jitter. Captured synthetic Shift/Ctrl/Alt modifiers are pulsed only when no
equivalent real modifier is held; their markers are validated across batches and
remain tracked for lease cleanup on native failure. Marked temporary modifiers
also reuse an already owned left or right native host modifier, including one
reached by remapping an ordinary source. The saved native plan supplies that
modifier identity across later policy changes. Real sources continue to share
only exact destinations, so opposite native modifier sides stay distinct. The
temporary source keeps its own cleanup ownership without changing the first
native plan or flags. This behavior has Linux fake
adapter coverage and Windows injected-sender test cases; Windows execution and
live desktop delivery remain qualification gates. Linux emits the same
repeat-event form through uinput. The opt-in grabbed Linux keyboard test checks
that form and stable release across mapping changes. Desktop typing and repeat
cadence on Linux and Windows remain qualification gates.

The `keybindings` value is an ordered flat list of source/destination virtual-key
bytes. It accepts exact integers from 0 through 255, an even number of values,
and at most 256 pairs (512 values). Floats, strings, negatives, wider integers,
and odd lists are rejected. Built-in generic modifiers always resolve first:
`0x10`→`0xA0`, `0x11`→`0xA2`, `0x12`→`0xA4`. Then the first custom pair for the
source wins; the Right Alt alias is considered last. Mapping applies once rather
than chaining through destination pairs. Native admission validates the resolved
destination against the selected platform and keyboard mode; valid policy bytes
do not imply every destination can be injected.

An acquire may send a nullable `keybindings` list. Omission or `null` follows the
live global list, including later durable global changes. A supplied list replaces
the global list for that lease, and `[]` retains only the built-ins and optional
Right Alt alias. Settings updates that omit the field retain the saved global
list; a present `null` is rejected, and an explicit `[]` is saved unchanged.
Acquire and operation lookup return the requested list as `keybindings`, preserving `null` and `[]`. Active status also
reports the effective lease list and its nullable `requested_keybindings`.
Operation replay requires the same ordered request list, including duplicate pairs
and the difference between inherited and explicit empty policy, for active leases
and terminal records. Successful lease cleanup restores the current global list.
Backend plans retain the native destination selected for each held source across
policy changes, so its repeat and release target remains stable.

## JSON API

```json
{"action":"status"}
{"action":"acquire","operation_id":"session.input.7","ttl_ms":5000,"native_pen_touch":true}
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

`native_pen_touch` is an optional acquire boolean. An omitted value uses the
durable global input setting; the upstream client/application policy resolver
passes its resolved scoped value as `Some(true)` or `Some(false)`. The effective
value is snapshotted for the lease and returned by Acquire, active status, and
operation lookup; callers use this lease value for capability behavior. Acquire
retries must keep the same optional value.
When enabled and supported, a lease can send `pen_touch` events through the
native backend. `pen_touch_supported` reports the backend capability separately
from this saved policy value.

Active acquire and lookup responses include the backend's `absolute_pointer`
and `pen_touch_supported` capabilities and the effective `native_pen_touch`
value. Callers must require the relevant capability before sending those event
types; terminal tombstones omit backend capabilities.

Batches contain 1–64 events. The entire batch is validated against backend
capabilities and a simulated pressed state before its first native event.
At most 128 keys/buttons and 16 pen/touch contacts may be held. Supported events:

- `key`: Windows requires `windows_scan` with a set-1 scan code from 1–127 and
  optional `extended`; Linux requires `linux_evdev` with a keyboard code from
  1–248, excluding `KEY_UNKNOWN` (240). These are native encoding ranges, not a
  portable key map; clients must choose a defined code for the target platform.
  Trusted protocol adapters may instead use `moonlight_vk` with a byte-sized
  Win32 virtual-key source. The selected list resolves that source once; native
  validation checks the destination against the platform and keyboard mode.
  Normalized packets use a fixed US physical mapping to produce the corresponding
  Windows set-1/extended or Linux evdev key. Generic and left Shift/Ctrl/Alt are
  distinct source identities whose native destination is shared through backend
  ownership. A lease cannot mix native and Moonlight key encodings, preventing
  two names from owning one native key independently.
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
- `pen_touch`: carries a `pointer_id` (`u32`), `device` (`pen` or `touch`),
  `phase` (`down`, `move`, `up`, or `cancel`), and normalized absolute `x`/`y`
  coordinates (`u16`, 0–65535). Touch pointer IDs retain the packet ID; pen
  uses the stable `pointer_id` 1 in its separate device namespace because the
  source pen packet has no ID. Optional pressure is finite and normalized to
  0–1. Touch and pen can carry paired `contact_area_major` and
  `contact_area_minor` values: each touchport-transformed desktop length is
  divided by 65535. Optional `rotation_degrees` preserves unknown as
  `null` and accepts known values from 0 through 359. Pen can also carry
  `pen_tool` (`unknown`, `pen`, or `eraser`), a `pen_buttons` bit mask
  (`primary` 1, `secondary` 2, `tertiary` 4), and raw `tilt` from 0 through
  90 degrees; absent values preserve protocol unknowns. Touch rejects pen-only
  metadata, and malformed, non-finite, or out-of-range values are rejected
  before injection. Backends convert the retained metadata to their native
  orientation, contact, tool, button, and tilt representations.

  When enabled for the lease, contacts are keyed by (`device`, `pointer_id`):
  Down must create a free identity, and Move, Up, and Cancel must refer to an
  identity held by this lease. A duplicate Down or unowned event is rejected
  before injection; a cancellation for another pointer is never synthesized.
  Repeated identical Move samples are coalesced. Policy-suppressed events
  remain accepted without creating contacts. Lease release, expiry, failure,
  device loss, and shutdown cancel only the exact contacts injected by that
  lease; failed cancellations stay owned and retry through the lease cleanup
  path.

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

Windows loads a vetted ViGEmClient DLL from the application directory or
System32, resolves the required SDK exports, and probes `vigem_connect` when an
input lease starts. It retains keyboard and mouse support if the DLL, required
exports, or ViGEmBus connection is unavailable. Active controller slots create wired Xbox 360 targets
lazily and receive complete XUSB reports. The XUSB report contains the protocol's
D-pad, Start/Back, stick clicks, shoulders, Guide, face buttons, two triggers,
and four signed axes; the protocol's misc button maps to XUSB Guide, while paddle
and touchpad bits without an XUSB equivalent are ignored. Successful removal
detaches the target and frees its SDK object. If removal fails, the slot and
target remain owned for retry; connection teardown
disconnects from ViGEm before freeing remaining SDK target objects. The backend
does not bundle or install a driver or SDK DLL, implement the VHF/UMDF protocol,
create DS4 reports, or process rumble/LED feedback. ViGEmClient's `XUSB_REPORT` ABI is represented as
a 12-byte C-layout record and is covered by pure mapping tests. Native Windows
device delivery still needs qualification with the actual ViGEmClient DLL and
ViGEmBus driver.

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
Wayland absolute-pointer integration, Switch reports, VHF/UMDF and Inputtino
integrations, rumble/LED feedback, and crash-surviving Windows
keyboard/mouse cleanup remain unimplemented.

The pen/touch event coordinates and contact dimensions arriving at this module
are expected to have already been transformed from client coordinates into the
captured desktop geometry by the protocol adapter. Contact lengths use the
reference transform's polar-to-cartesian axis scaling, divided by the same 65535
coordinate span used by `x` and `y`; native adapters restore that length scale
without applying the touchport transform a second time. The Linux backend
reports touch orientation through its signed ABS_MT orientation range; Windows
derives touch contact bounds and pen tilt axes from the retained rotation and
tilt when those values are known. Native pointer ranges and contact geometry
still need qualification on supported operating-system versions and input
stacks.

The current GameStream decoder still treats `SS_TOUCH` and `SS_PEN` packets as
unsupported, so remote clients cannot reach this event path yet. The protocol
adapter must add event decoding, lifecycle mapping, capability checks, and the
client-to-desktop coordinate/contact-area transforms before end-to-end input is
available.

## Validation and reference

Tests use fake backends for ownership, stale tokens, batch admission, chord
release order, cancellation, partial failure, expiry, retained failed releases,
retry and shutdown, including pen/touch identity, multiple contacts, duplicate
events, device loss, and contact cleanup. Platform mapping tests inspect native
event records without injecting input or creating a uinput device. Default tests
never inject input or create a uinput device on the developer desktop. The ignored
`native_uinput_xbox_slot_reports_readback_and_teardown` test creates one real
Xbox-like virtual controller, resolves its exact event node using `UI_GET_SYSNAME`,
and obtains an exclusive evdev grab before sending any reports. It checks native
button/axis frames, kernel key/axis state, a neutral report, and destruction of a
slot with a held button. The controller is destroyed before its grab is released,
including during panic unwinding. Native write failures end the test instead of
replaying reports to an ungrabbed replacement. It creates no keyboard or pointer device and
changes no permissions. Run it explicitly with:

```sh
cargo test -p amaterasu-input native_uinput_xbox_slot_reports_readback_and_teardown -- --ignored --nocapture
```

The ignored `native_uinput_keybinding_targets_keep_source_ownership_and_teardown`
test creates only one virtual keyboard and grabs its exact owned evdev node
before sending any input. Independent evdev reports and kernel key-state queries
check single mapping, multiple source ownership of one target, held repeat and
release after policy replacement, built-in/custom Right Alt priority, reserved
source bytes, and destruction while a mapped key remains held. Every injection
failure ends the test before recovery could create an ungrabbed replacement.
Run it explicitly with:

```sh
cargo test -p amaterasu-input native_uinput_keybinding_targets_keep_source_ownership_and_teardown -- --ignored --nocapture
```

Both tests require access to `/dev/uinput` and to their exact owned device's evdev
node. A kernel readback pass establishes this bounded native device path; it does
not establish service-account permissions, game visibility, or client delivery.
Native pen/touch delivery, Windows and Linux compositor behavior, and hardware device
loss remain qualification gates. GameStream ingress and its client-to-desktop
coordinate transform remain pending in the protocol adapter.

Ordered keybinding priority and whole-list scope selection use Vibeshine commit
`457237db7e57f9c4be21c35bb07e28d1c100eb7d` as a behavioral reference. Amaterasu's
bounded byte-pair contract deliberately rejects wider integers accepted by that
reference parser, without truncating them.

Vibeshine commit `aa292c4cb097f22beab19d6ebf0307367f9bf449` was a behavioral
reference, including its keyboard/mouse interfaces and history concerning
captured-monitor absolute motion and release ownership. No implementation source
was copied or translated; captured-monitor mapping is an explicit remaining gap.

References: [Windows SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput),
[MOUSEINPUT coordinates](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput),
[Linux uinput guide](https://kernel.org/doc/html/latest/input/uinput.html),
[Linux v6.12 uinput UAPI](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/uinput.h),
[Linux v6.12 event codes](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/input-event-codes.h).
