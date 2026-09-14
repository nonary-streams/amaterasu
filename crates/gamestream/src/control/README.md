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
sensitive: a host RGB LED command is not a client FEC report. Other host messages
remain unsupported. Start/IDR recognition does not claim streaming readiness.

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
`NON_NORMALIZED` flag. The translator uses a fixed US physical-key table shared
with the input service. Generic and left Shift/Ctrl/Alt aliases share one logical
key; duplicate downs become repeats and duplicate ups produce an empty batch.
Missing Shift/Ctrl/Alt modifiers around an ordinary down are emitted as a bounded
synthetic chord. A missing Meta modifier, a non-normalized or unmapped key, text,
controller state, and haptics return an explicit translation error.

Wire mouse buttons map as 1 left, 2 middle, 3 right, 4 back, and 5 forward.
Relative `-32768` is rejected because the input service's symmetric range ends at
32767. Scroll amounts accumulate independently by axis in the protocol's 120-unit
wheel detents; full steps are split into events of at most 100 steps. Thus one
authenticated input record yields a batch of at most seven service events, and it
may deliberately yield none for a duplicate release or partial wheel detent.

Absolute coordinates are not inferred from the selected stream resolution.
Translation requires an authoritative `AbsoluteViewport` expressed as an
inclusive rectangle in Windows virtual-desktop normalized coordinates. Given that
trusted rectangle, client coordinates and reference dimensions are checked and
scaled with integer rounding. Without it, absolute input is explicitly
unsupported; the current stream-session integration does not supply one.

Touch, pen, controller arrival/motion/touch/battery, host rumble/LED/HDR feedback,
legacy startup/input, loss-stat messages, and old termination variants are not
implemented. Unsupported input records remain bounded by the enclosing control
payload limit. Text is at most 32 bytes; the primary client normally sends one
complete Unicode code point per packet. NUL and malformed UTF-8 are rejected.

The primary source evidence is Moonlight `moonlight-common-c` commit
`874ac9548f1bd6f095ef2b435c42cdde460e7821`, `src/ControlStream.c`,
`src/InputStream.c`, `src/Input.h`, `src/Video.h`, and the feature declarations in
`src/Limelight-internal.h`. The behavioral baseline is Vibeshine commit
`aa292c4cb097f22beab19d6ebf0307367f9bf449`, inspected through `git show` for its
control dispatch and encrypted-only admission. No source was copied or translated.

`generate_vectors.py` independently constructs mixed-endian wire fixtures using
Python's standard `struct` module. `--check` verifies the checked-in fixture file.
Rust tests decode those fixtures and reject every fixed-record truncation,
trailing bytes, reserved-field corruption, wrong direction, malformed text, and
oversized payloads. These are protocol decoding checks; live device dispatch and
complete-client session qualification remain separate integration gates.
