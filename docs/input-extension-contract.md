# Optional input extension boundary

`amaterasu.input-extension/1` is an optional, separately composed broker service.
The Rust SDK in `amaterasu_module_sdk::input::extensions` supplies a bounded
message type, a consumer submit helper and a provider adapter. It does not
change the v1 input lease, GameStream wire format, or native ABI. Nothing in
this contract enables an OS device, a network protocol or a client feature.

The trusted lifecycle/composition owner installs a `RoutePolicy` for each
host epoch, session, authenticated source, logical device and generation. It
supplies separately authorized immediate caller and initiator pairs for traffic
to the device and to the client. Neither route values in a request nor a
capability name grant access. This adapter checks both broker-authenticated
identities and an exact live route, and it rejects expired requests. The caller
must use the broker's cancellation check before dispatch. Composition is
responsible for authenticating the lifecycle evidence, binding it to the input
lease and client, and retiring a route before those rights end. There is no
production lifecycle integration in this increment.

Each capability is an exact namespaced schema, version and direction. Installation
intersects provider and protocol-adapter offers, verifies required capabilities
are in the intersection, and asks the endpoint whether it implements each of
its advertised capabilities. This is a provider assertion, not independent
proof of behavior. Unknown required capabilities must be rejected by either
actual implementation before they are offered. Protocol negotiation with a real
client and per-device availability checks remain the respective adapters' jobs.

A message carries at most 2,048 payload bytes. The maximum serialized request is
16 KiB; routes, capability lists and principals are bounded. `Endpoint::validate`
checks the schema-specific payload before admission or replacement. Two optional
schemas are included, each with an independent capability and SDK codec:

- `nonary.steam-controller.attributes/1` encodes transport, valid mask, six
  little-endian `u32` values and three fixed 16-byte strings. The authenticated
  route supplies device identity. Firmware time is in Unix seconds; the units of
  other build times are not specified by this contract. The bounded serial fields
  may identify hardware and must not be logged or persisted by default.
- `nonary.controller-speaker.pcm/1` encodes a nonzero owner and producer
  generation, source sequence, sample-frame position, flags, and 960 bytes of
  interleaved stereo S16LE at 48 kHz: 240 sample frames, or 5 ms. Only the source's
  start flag is defined. It is independent of actuator haptics.

The field definitions derive from Vibeshine commit
`eeab9d96c4db45de511022d451810dd2c76510f3`, `src/steam_controller_protocol.h:118-154`,
`src/controller_speaker_pcm.h:14-25,95-136`, and
`src/platform/windows/dualsense_usbip_gamepad_policy.h:118-150`. These SDK
encodings are not the client wire packets. A compatible protocol adapter must
separately implement and negotiate the exact wire format, check controller type,
capability, session feature, owner and generation, and preserve the established
cryptographic boundary. Schema support alone grants no device or client access.

Controls use contiguous admission sequence numbers per route/direction beginning
at 1. A trusted route policy can classify particular negotiated capabilities as
sampled: their admission sequence is monotonically increasing per capability and
may contain gaps, independently of control and the source's PCM sequence. A
sample lane keeps at most one unsent message per route/capability and 32 total,
replacing only the matching slot. It drops expired pending samples using a
trusted configured local age of 1–1000 ms, and reports busy without consuming a
sequence when no slot is available. The owner must schedule bounded, fair drains;
no worker or wakeup is created by the helper. A busy endpoint retains the sample
until expiry, while a rejected endpoint drops it. Provider policies may declare
which accepted terminal controls fence which sample lanes: the adapter removes
pending samples and rejects later ones until a fresh route generation is installed.
This is local queue fencing; endpoint work already submitted must be retired by
its owner and checked again at the final send. A PCM sequence or sample-position
gap signals a source discontinuity; no new client wire flag is invented.

`Endpoint::submit` must return busy without consuming the message. Success means
only local admission, not OS injection, remote delivery or playback. Endpoint
queues, terminal controls, expiry and worker retirement remain separately owned.
Broker retry records and operation IDs must be reconciled for ambiguous replies;
retrying a potentially accepted message under a new operation ID is not
idempotent. The adapter does not persist admitted work or cancel work already
submitted. Native providers remain trusted code.

The owner serializes dispatch, installation and retirement. Retirement fences
new admission and does not cancel already admitted work. A replacement for the
same logical identity must use a higher generation; tombstones are retained for
the adapter lifetime and bounded to 32 identities. Restart recovery and
host-epoch generation require trusted lifecycle coordination and are not
implemented by this adapter. When its bound is exhausted, the adapter rejects
new identities rather than evicting tombstones and risking stale reuse.

`crates/runtime/src/input_extension_tests.rs` exercises two synthetic providers
through the real selected broker, including both directions and unauthorized
callers, unknown capabilities, stale routes, oversized or malformed payloads,
reliable control ordering, latest-sample pressure, expiry, terminal fencing and
retirement. It neither loads a production provider nor proves the actual
Steam/speaker wire protocols, driver access, timing or client interoperability.
The existing `authenticated_controller_extension_packets_reach_the_native_input_contract`
stream-session test covers arrival, touch, motion and battery through the
production pipeline's explicit pre-translator path; the standalone translator's
rejection of those events is not a production-path rejection.

## Lifecycle source-label checkpoint

The optional checkpoint at lifecycle commit `0fb7f221b918565fdad7764daa4fd453d90d12dc`
returns `source: {version: 1, id, generation}` only when the selected input
consumer requests `source_version: 1` in `validate_input_binding`. A null source
means no attested active source. Omitting the version preserves the existing
response shape; `paired_client` remains null. `source_label::observe` is a pure
parser for that negotiated response against an expected stream target. It does
not send a broker request, authorize anything, or install a route.

Do not connect this parser to production effects yet. A lifecycle label and
revalidation alone do not close the asynchronous protocol revocation race. The
protocol-owned effect admission and acknowledged revoke/drain barrier must be
implemented and tested first. The app/lifecycle worker owns that authority.

Once its exact operation is available, the input owner must bind the exact host
epoch, session ID, stream ID and stream generation, lifecycle source ID and
source generation, selected input principal, provider boot epoch, and separately
managed device identity/generation to an admitted route. The current `Route`
contains only host epoch, session, source ID, device ID and one generation; it
must be versioned or extended to distinguish these incarnations before use.
Do not substitute remote-supplied identifiers or treat a source label as a bearer
token. Pause, revocation, expiration, cleanup and restart must fence new effects
before draining already-admitted work. Unknown barrier outcomes remain fenced.
