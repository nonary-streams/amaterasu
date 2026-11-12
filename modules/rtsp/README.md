# Managed encrypted RTSP

This library owns the RTSP listener and negotiation lifecycle. It delegates media
allocation, authenticated startup and cleanup to `amaterasu.stream-session/1`.
The native shell has no protocol-specific behavior. The service is
`amaterasu.rtsp/1`; the platform and synthetic stream-probe packages include it
with networking disabled and an empty caller allowlist.

A trusted caller provisions a ticket with an already authorized peer, fresh key,
either fixed media destination ports or UDP peer discovery, capture selection and
a fixed media profile. Ticket knowledge does not grant another broker principal ownership.
Network requests must prove key possession through encrypted RTSP envelopes and
match the ticket's peer address. Keys never appear in replies or events.

The control actions are `open`, `close {ticket}`, `status {ticket?}` and
`lookup {operation_id}`. Open takes `operation_id`, `peer_ip`, `video_port`,
`audio_port`, `key_hex`, `key_id`, `connect_data`, `capture`, optional
`device_name`, `profile`, and `discover_peers` (default false). `device_name` is
Moonlight's bounded session label; it is passed separately from capture `output`.
Fixed mode requires both ports.
Discovery mode requires them to be omitted or zero so caller-supplied ports cannot
silently override discovery.
Capture contains `capture_provider` (`dxgi`, `wgc`, `pipewire`, `x11`, `kms`,
or a configured external alias), optional `constant_rate`
(default false), `output`, `encoder`, `audio_endpoint` and optional
`host_audio_enabled` (default false). That flag snapshots Moonlight's
`localAudioPlayMode` choice and is forwarded through stream-session to AudioCapture.
Top-level `channels` may request 2, 6 or 8 stream channels for a regular
`audio_endpoint` when `stream_audio` and `capture_sink_without_routing` are true.
This requests capture-format conversion without selecting a virtual sink layout;
`virtual_sink` remains absent and the selected host playback policy is preserved.
External aliases are 1–64 lowercase ASCII letters, digits, underscores or hyphens;
Video resolves them against its configured providers and rejects unknown aliases.
Constant-rate capture is accepted only with WGC and is forwarded to stream-session
and Video as part of the per-open selection. The profile supplies dimensions, frame rate, bitrate in kbps,
reference-frame limit, video packet/FEC settings, Opus packet duration/bitrate and
encryption policy. Unknown fields are rejected; network data cannot override these
authorized settings.
NVHTTP also supplies `profile.video_max_batch_size_kb` for each launch; direct
RTSP opens default it to 64. The effective 16/32/64 value is copied to the nested
`video.max_batch_size_kb` stream-session setting.
The ticket peer and selected listener must use the same address family; an
unreachable ticket is rejected before media allocation.

NVHTTP is the only caller allowed to use the private `network_configure {port}`
action. For a valid base port in 1029..=65514, it configures this listener at
base+21 and passes per-stream local UDP binds at base+9 (video), base+10 (control),
and base+11 (audio) to `stream-session`. The existing listener IP is preserved.
The action defers while a ticket is live or an accepted TCP connection remains,
and reports active only after the replacement RTSP listener binds and all three
UDP addresses pass a simultaneous bind check. UDP sockets themselves belong to
per-session stream owners, so the check does not reserve ports between streams.
Pending updates keep new opens on the currently bound listener's base until retry
succeeds.
An occupied unrelated target leaves the old TCP listeners bound. Same-port
overlap can require releasing the old set; if replacement and restoration both
fail, status clears the active tuple and addresses and retries the requested
policy instead of reporting cached sockets. Live tickets and unconfirmed cleanup
continue to defer listener changes.
This does not alter NVHTTP's HTTP/HTTPS listener or weaken its loopback policy.

Startup settings accept `address_family` (`ipv4` or `both`) and `bind_address`
(an IP address or empty string). A nonempty `bind_address` replaces the RTSP
listener IP and supplies the same-family video, control and audio UDP bind
addresses while preserving configured ports. When both policy keys are omitted,
the legacy `bind` IP remains in effect. An explicit `address_family` supplies
family defaults when `bind_address` is omitted or empty: loopback addresses when
remote access is disabled, and family wildcards when remote access is enabled.
For wildcard listeners, RTSP selects a concrete local route for each peer and
uses it in the returned URL and that stream's UDP binds.

With an explicit IP, RTSP and derived UDP binds use that one family. With an
explicit empty address and `both`, RTSP binds separate IPv4 and IPv6 TCP sockets
on the same port, and UDP preflight checks both family-specific video, control,
and audio ports together. IPv6 sockets are v6-only, so IPv4 reachability does not
depend on host dual-stack defaults. When remote access is disabled, both sockets
are loopback-only; otherwise each family uses its wildcard. Wildcard RTSP selects
a concrete same-family local route for each peer and uses it for that ticket's URL
and UDP binds. Status retains the compatibility `local` field for the primary
listener and adds `locals` containing every bound address.
The shared `amaterasu-network-policy` library also binds actual media/control UDP
sockets with the same v6-only behavior. Each ticket freezes its requested UDP
endpoints; prepare readback must preserve their IP/family and fixed ports before
SETUP can advertise them. Provider mismatches retain cleanup ownership until the
existing stream-session cancellation and release workflow settles.

Replies expose an opaque ticket, bound address, RTSP URL, protocol `state`,
downstream `stream_state`, and fixed failure/cleanup codes. `playing` means PLAY
was accepted; `stream_state: active` separately indicates media startup completed.
Two open tickets and 128 retained operation records bound allocations. A ticket
is open from admission until it is asked to close; one that is closing or failed
to close counts against neither bound. When the record table is full, ended
tickets are dropped to make room, the most settled first, and their broker calls
are handed to settlement. Each open ticket holds one media port slot. A slot
whose ticket ended without confirming cleanup is reused only when no slot is
free, starting with the ticket that ended longest ago. Each ended ticket runs
its cleanup in a job of its own, so a stop that is slow or never answered does
not delay reading requests, preparing the next ticket or another ticket's
cleanup. Lists show at most 16 owned records and report truncation. Identical open retries preserve the
original ticket; changed input conflicts, including after the ticket closes.

The initial profile is H.264 SDR with limited-range BT.709, stereo 48 kHz PCM and
5/10 ms Opus. The prepared profile must match ANNOUNCE, including reference-frame
constraints, packet sizing, FEC and encryption. An optional single-reference
constraint is forwarded to the encoder and requires explicit backend confirmation.
The trusted profile also forwards `permit_legacy_audio_iv_wrap`, default false,
so installed policy can explicitly select the transport's existing legacy
long-session audio behavior. This does not add authentication to CBC audio;
see the [media transport contract](../media-transport/README.md).

## Client ordering

The modern transcript is OPTIONS, DESCRIBE, SETUP for audio/video/control,
ANNOUNCE, then PLAY. Session tokens, targets, CSeq progression, launch epochs and
pending completion tickets are checked explicitly. SETUP reports the allocated
server ports. ANNOUNCE acceptance commits only a supported prepared profile.

The first two simultaneous transports on an address use UDP ports at base +9
through +14. Remote clients need inbound access to that range; existing rules
for only base +9 through +11 cover one client. Further admissions fail until
a slot is available. A reserved slot is reused only after its prior transport
confirms cleanup.

PLAY arms the owned media session. It does not claim capture is already active:
Moonlight opens control after RTSP completes. The session library waits for
control authentication before opening capture, with bounded startup and rollback.
RTSP TEARDOWN and trusted ticket close require confirmed session cleanup.

One encrypted RTSP request is accepted per TCP connection, followed by an encrypted
response and EOF. Crypto counters and replay windows live on the ticket across
connections. There is no plaintext fallback. See the
[transport contract](src/transport/README.md) for bounds and disconnect semantics,
and the [negotiation contract](../../crates/gamestream/src/rtsp/negotiation/README.md)
for protocol state and compatibility limits.

## Qualification and remaining integration

`scripts/test_rtsp.py` exercises the signed synthetic-source composition with an
independent Python AES-GCM RTSP client, the pinned ENet C client, real Opus and
independently checked media encryption/FEC. Synthetic video is a framing fixture,
not a decodable H.264 stream or evidence of hardware behavior.

Moonlight's `X-GS-ClientPort` setup field remains a placeholder and is never
accepted as destination authorization. In discovery mode, RTSP creates distinct
16-character base64url audio and video bearer tokens from 12 random bytes each,
for 96 bits of entropy per lane. Only the corresponding encrypted SETUP response
receives each `X-SS-Ping-Payload`; ticket responses and status never expose them.
The downstream media transport validates `SS_PING` and freezes each lane's source
port. NAT rebinding after that point is not supported. Public NVHTTP launch,
application and display transactions, paired-client revocation wiring,
reconnect/resume, OS input dispatch, pacing and actual client/hardware qualification
remain required.
The HTTP adapter continues to report streaming unsupported.

Orderly ticket close must precede replacement for confirmed cleanup. Synchronous
module unload can release sockets and settle bounded broker work, but cannot drive
asynchronous provider teardown after ticking stops. Provider unload/process exit
is the backstop for active resources; native grants do not sandbox trusted code.

Encoder selection preserves legacy unsigned startup ordinals and accepts a full
stable encoder identity or the exact `"auto"`/`"software"` backend selectors.
The selection passes unchanged to video, which resolves non-identity selectors
once before capture allocation. Missing software support fails explicitly; fixture
control success does not qualify real capture, encode, or client playback. See
[video selection policy](../encode-video/video/README.md) for bounds and automatic-selection
limits.

Software tuning remains exact through the launch snapshot: nullable `sw_preset`
and `sw_tune` inherit defaults, while explicit empty strings are retained.
RTSP freezes signed `min_threads` and QP with the global/application/client
precedence before opening the stream. ANNOUNCE contributes the client slice
count; software requests `max(videoEncoderSlicesPerFrame, min_threads)` and
passes it to the video encoder, subject to x264 frame-size and backend limits.
QP defaults to 28 and remains metadata because
x264 ABR ignores it and Amaterasu has no VAAPI CQP consumer. The backend policy
supports `auto` and `software`, and real codec availability remains a separate
requirement. [NVHTTP policy](../nvhttp/README.md#software-encoder-policy)
documents persistence, scope precedence and platform qualification limits.


## Retained-source retirement observations

Trusted startup selects `retirement_status_owners`, mapping each observer to one
original transport owner, for observation only. The older
`retirement_status_principals` list is accepted only when every listed observer
has an explicit owner mapping; an unscoped observer policy fails validation.
They may call `amaterasu.rtsp/1` with
`{"action":"retirement_status","operation_id":"opaque-transport","expected_runtime_epoch":"32-lowercase-hex-digits"}`.
Both selectors are optional for a startup snapshot. Replies identify
`runtime_epoch`, `startup_reconciled`, `operation_id`, `state`, `retired`,
`active_operations`, `pending_admissions`, and `cleanup_pending`. Observers never
receive stream keys, peer addresses or private ticket URLs. Normal transport
owners see only their operations; an installed retirement observer sees only its
selected owner's operations, admission fences, and occupancy counts. Requests
cannot select another owner. Each selected owner must already have an installed
transport admission grant.

Persist the observed runtime epoch before admitting a retained-source transport,
then send it as `expected_runtime_epoch` on Open. Late requests for a previous
runtime are rejected before effects. Missing current-epoch tickets report
`unknown`, never retirement. A different saved epoch reports `epoch_fenced` only
after the downstream durable cleanup journal and RTSP's native startup cleanup
have reconciled. Existing tickets override epoch fencing until exact native
retirement is confirmed. A snapshot with `active_operations: 0` proves only the
scoped operations currently known to this runtime; also require reconciled
startup, no pending admission and no pending cleanup. `startup_reconciled` names
the prior-runtime native recovery barrier: current-epoch admission or cleanup
does not make a fresh epoch observation unready. Its separate occupancy fields
still describe the exact selected owner's current work.

A normal owner may call `retire_operation` with the exact operation and saved
epoch. It installs an owner-bound admission fence before looking up effects,
then drives real close/cleanup for an existing ticket. Only an operation that
never entered this runtime can immediately report `operation_fenced`; late Open
requests cannot pass that fence. Fences remain for the runtime lifetime and are
bounded at 128; capacity exhaustion rejects another fence without forgetting old
ones. Observation-only principals cannot call this operation. After a failed
startup pass has settled, a normal owner may call `reconcile_startup` to retry
its native reconciliation; this never marks cleanup successful by itself.

Startup reconciliation validates the complete broker-scoped stream-session
snapshot before stopping an exact owner. While the downstream startup journal
is still unresolved, it retries only records explicitly marked `cleanup_failed`;
ordinary interrupted workflows keep their existing automatic cleanup owner.
Each owner is attempted at most once in a bounded reconciliation pass. Failed
cleanup or a later contradictory live observation blocks the pass until an
authorized owner requests another reconciliation. Successful Stops are followed
by a fresh global snapshot: epoch retirement remains fenced until the durable
startup barrier is reconciled and no session of an earlier runtime remains.
Retained broker settlement continues using its exact request identity after
transient transport failures; settling that bookkeeping alone leaves the pass
blocked and requires an explicit reconciliation retry.

Reconciliation is cleanup of an earlier runtime's work and does not decide
admission. A new ticket waits for it for at most ten seconds after start,
because those sessions may still hold the media ports, and is answered
`startup_reconciliation_pending` during that wait. Once the pass completes,
blocks or the ten seconds pass, tickets are admitted and the startup snapshot
reports `state: ready`; `startup_reconciled` separately keeps saying whether
the earlier sessions were retired, and a blocked pass logs its reason once.
Sessions that belong to this runtime's tickets are recognized by their
operation or session identifier and are never stopped by the pass. If the
stream-session provider does not report the operation and a ticket is still
being prepared, the pass looks again instead of guessing.

Signed startup `max_live_tickets` and downstream `max_live_sessions` are bounded
from 1 to 32 and default to 2. Configured media endpoints reserve three UDP
ports per concurrent ticket, starting at the usual video/control/audio offsets
and continuing in blocks of three. Slots remain reserved through uncertain
cleanup; port arithmetic fails closed at the u16 limit. The cleanup journal can
retain all 32 admitted sessions even when a later startup policy lowers capacity.

Borrowed capture uses the public `BorrowedCaptureContext` under the
`borrowed_capture` field. RTSP passes the frozen context to stream preparation
and the lifecycle coordinator, requires external capture ownership, binds it to
the exact operation/epoch/output, and rejects display mutation admission. The
stream consumer checks the unchanged context before opening its exact source.
App-independent streams report authenticated client disconnect only after exact
native retirement and a control witness bound to their owned stream operation.


The optional top-level `source_capture_context` carries a public Capture SDK
logical route and optional actual source mode. RTSP freezes it through Prepare
and Start; stream-session forwards it unchanged to Video Open, outside encoder
configuration. The installed video provider resolves the logical route's provider
and source class. A nondefault borrowed route requires the context's explicit
source mode to match its frozen retained geometry and range. Missing context
preserves the legacy default route; unknown source mode or depth does not imply
eight-bit capture. Changing the context after admission fails before capture.

RTSP closes new Open and PLAY startup admissions when the host announces shutdown,
stores the original absolute deadline, and preserves existing transport and native
ownership until its dependent consumers drain. Cooperative module quiescence then
revokes pending RTSP connections and drains existing stream ownership
through the ordinary asynchronous cleanup workflow while providers still tick.
Every following lookup and stop shares the first cleanup deadline. A failed
cleanup or unresolved broker call keeps quiescence incomplete; repeated polls do
not reopen an undo attempt or grant another cleanup budget. Drop does not restart
cleanup after a host shutdown announcement or cooperative quiescence has begun.

The synchronous fallback shutdown also uses one aggregate cleanup deadline.
Interrupted lifecycle preparation is recovered by an exact authenticated lookup
and the same target, capture binding, and shared source validation used for live
startup before a cleanup-only Stop is sent. A known coordinator result is retained
before broker result release, so bookkeeping failures cannot discard its target.
Read-only observation settlement is retained separately from native retirement.
The same separation applies during normal operation: a disconnect can start its
owned Stop while an earlier periodic status observation is still being settled.
Observations after a submitted Stop continue polling that cleanup instead of
submitting another Stop. Pending observation settlement is bounded and retried.

Canceling a broker call before its first submission sends no request. Only that
local proof, or a first explicit pre-enqueue broker rejection, allows an
unsubmitted lifecycle preparation to be classified as not admitted. An admission
conflict, a lost admission response, or a later rejection preserves uncertainty.
A never-submitted lookup says nothing about its original preparation. In
particular, an accepted call canceled before provider delivery cannot currently
be distinguished from a committed call using Cancel/Release acknowledgments;
plain same-epoch lookup absence remains fail-closed.
