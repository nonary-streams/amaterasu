# Authenticated inner control and input decoding

`decode(Profile::SunshineModern, Direction::ClientToHost, kind, payload)` borrows
an already authenticated modern control message and returns a typed `Message`.
It performs no socket access, input injection, keyframe request, or session
mutation. `Profile::Legacy` is explicitly rejected. The caller must establish
Sunshine modern control encryption and application version >= 7.1.431 during
negotiation; selecting this enum does not establish either fact.

Only pass plaintext returned by the modern control GCM opener. The pinned client
sends the complete input header and payload inside that envelope without another
input cipher. Legacy separately encrypted input CBC/GCM framing is unsupported.
ENet channel and delivery flags are outside the authenticated inner payload and
must never grant authority. The caller must associate the authenticated key with
an authorized session and enforce input leases and action policy independently.
Unknown message kinds and input magic values retain borrowed bytes in
`Unsupported`; they never become active input. Malformed records are errors.

| Client-to-host kind | Typed record | Exact payload bytes |
| --- | --- | --- |
| `0x0200` | Ping: LE16 length 4, LE32 timestamp, opaque tail | 8 |
| `0x0307` | Start (zero byte) | 1 |
| `0x0302` | Request IDR (two zero bytes) | 2 |
| `0x0301` | Reference invalidation: LE32 first/last, reserved zero words | 24 |
| `0x0350` | Long-term reference acknowledgement: LE32 frame, reserved zero | 8 |
| `0x5502` | Big-endian frame/FEC feedback | 21 |
| `0x0206` | Input packet | Variable, <=65,511 |

The primary ping producer writes only six of its eight bytes; the decoder
preserves the final two bytes without interpreting them or requiring zeroes.
Host-to-client `0x0109` contains an exact four-byte big-endian termination reason.
That kind is rejected as `WrongDirection` from clients. `0x5502` is direction
sensitive: the host uses it for RGB LED output while the client uses it for FEC
feedback. This decoder recognizes termination only in the host direction;
`encode_host_feedback` separately validates and builds the typed feedback records
below. Start/IDR recognition does not claim streaming readiness.

`HostFeedback` is a strict Serde-tagged input/output value (`type`, snake_case),
addressed by client-relative controller slot. `encode_host_feedback` validates
slots and PCM bounds, then creates the inner type and body; it does not create an
encrypted envelope or sequence number. The authenticated control transport owns
both, and ENet channel/delivery remain routing metadata outside GCM.

| Host-to-client kind | Typed record | Payload and delivery |
| --- | --- | --- |
| `0x010b` | Rumble | LE32 `0x00c0ffee`, LE16 slot, low/high LE16; reliable channel 0 |
| `0x5500` | Trigger rumble | LE16 slot, left/right LE16; reliable channel 0 |
| `0x5501` | Motion state | LE16 slot, report rate LE16, motion type byte; reliable channel 0 |
| `0x5502` | RGB LED | LE16 slot, RGB bytes; reliable channel 0 |
| `0x5503` | Adaptive triggers | LE16 slot, flags/types, two opaque 10-byte effects; reliable channel 0 |
| `0x5601` | Haptics PCM | Version/format bytes, LE16 slot, LE32 sequence, LE16 frame count, zero reserved LE16, then 4 bytes per frame; sequenced-unreliable channel 8 |

PCM uses version 1, format 1 (48 kHz signed 16-bit little-endian stereo),
1–240 frames, and an exact `12 + 4 * frames` payload size. The caller must also
enforce negotiated global PCM support and the target controller slot's advertised
capability before enqueueing it; encoding alone does not authorize playback.

Input packets begin with BE32 length excluding that length word and LE32 magic.
Supported input includes key down/up (flags, LE key code, modifiers), relative
mouse (signed BE coordinates), absolute mouse (BE coordinates and inclusive
reference maximums converted to pixel counts), five mouse buttons, signed
vertical/horizontal scrolling, bounded complete UTF-8 text, haptics enable, and
16-slot multi-controller state with 32-bit Sunshine button flags. Fixed sizes,
reserved constants, duplicate scroll amounts, coordinates, and controller slots
are validated. Unknown keyboard flag/modifier bits are rejected. Key codes and
controller button bits are preserved for downstream mapping; no mapping or
platform input effect is implied.

`InputTranslator` is the separate, stateful bridge from authenticated input
records to owned input-service events. Normalized keyboard packets must use the
primary client's `0x80xx` virtual-key form and a zero Sunshine
`NON_NORMALIZED` flag. Both forms preserve their original low-byte virtual key
and carry an explicit mode bit to the input service; the Windows backend
uses the configured current-layout scancode policy, while Linux continues to
use its fixed US physical-key table. Generic and left Shift/Ctrl/Alt sources
retain distinct ownership. Duplicate downs and unmatched ups produce an empty
batch. The input service resolves one configured mapping, checks whether the
native mode supports its destination, and coalesces sources that own the same
host key until their final release. It keeps the first native plan for held
targets across policy changes. The translator remembers the keydown mode for
release even if later packets change form. Missing Shift/Ctrl/Alt modifiers
around an ordinary down are emitted as a bounded synthetic chord. A missing Meta
modifier, text, controller state, and haptics return
an explicit translation error.

Wire mouse buttons map as 1 left, 2 middle, 3 right, 4 back, and 5 forward.
Relative `-32768` is rejected because the input service's symmetric range ends at
32767. Nonzero scroll amounts pass through as raw signed 16-bit wheel units; the
input service applies the global option and performs per-lease 120-unit detent
accumulation when high-resolution scrolling is disabled. A zero scroll amount
yields no event. One authenticated input record yields a batch of at most seven
service events, and it may deliberately yield none for a duplicate release or
zero-delta scroll record.

Absolute coordinates are not inferred from the selected stream resolution.
Translation requires an authoritative `AbsoluteViewport` expressed as an
inclusive rectangle in Windows virtual-desktop normalized coordinates. Given that
trusted rectangle, client coordinates and reference dimensions are checked and
scaled with integer rounding. Without it, absolute input is explicitly
unsupported. Stream-session supplies a viewport only when video reports valid
capture geometry and the acquired input backend supports absolute pointers.

Authenticated `SS_TOUCH` and `SS_PEN` records are decoded with exact sizes and
preserve touch pointer IDs, the fixed pen identity, contact fields, and pen
metadata. Hover remains unsupported. Stream-session forwards these records only
with an active input lease, native pen/touch capability and policy, and valid
`touch_port` metadata from video. Windows DXGI and WGC capture can now supply
that mapping for verified physical monitor topology through video; other or
unverified capture paths omit it and leave pen/touch input disabled. Live device
and coordinate qualification remains pending.

Controller arrival/motion/touch/battery decoding and host feedback encoding are
implemented as typed protocol values. Native producers, per-slot capability
tracking, authenticated session polling, and real client callbacks remain
separate integration gates. HDR feedback, legacy startup/input, loss-stat
messages, and old termination variants are not implemented. Unsupported input
records remain bounded by the enclosing control payload limit. Text is at most
32 bytes; the primary client normally sends one complete Unicode code point per
packet. NUL and malformed UTF-8 are rejected.

The configuration task pins Vibeshine commit
`457237db7e57f9c4be21c35bb07e28d1c100eb7d`; its Moonlight submodule is
`Nonary/moonlight-common-c` commit
`4ea115e8d23d1ca055a2aa8ddcfb601b705b3323`. The output record bodies and
reliability routing were checked against Vibeshine `src/stream.cpp` and the
matching Moonlight `src/ControlStream.c`, `src/ControllerHaptics.h`, and
`src/Limelight.h`. The prior primary-client decode baseline is Moonlight
`874ac9548f1bd6f095ef2b435c42cdde460e7821`. No source was copied or translated.

`generate_vectors.py` independently constructs mixed-endian wire fixtures using
Python's standard `struct` module. `--check` verifies the checked-in fixture file.
Rust tests decode those fixtures and reject every fixed-record truncation,
trailing bytes, reserved-field corruption, wrong direction, malformed text, and
oversized payloads. These are protocol decoding checks; live device dispatch and
complete-client session qualification remain separate integration gates.
