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
`audio_port`, `key_hex`, `key_id`, `connect_data`, `capture`, optional `profile`,
and optional `discover_peers` (default false). Fixed mode requires both ports.
Discovery mode requires them to be omitted or zero so caller-supplied ports cannot
silently override discovery.
Capture contains `capture_provider` (`dxgi` or `wgc`), `output`, `encoder` and
`audio_endpoint`. The profile supplies dimensions, frame rate, bitrate in kbps,
reference-frame limit, video packet/FEC settings, Opus packet duration/bitrate and
encryption policy. Unknown fields are rejected; network data cannot override these
authorized settings.
The ticket peer and explicit listener must use the same address family; an
unreachable ticket is rejected before media allocation.

Replies expose an opaque ticket, bound address, RTSP URL, protocol `state`,
downstream `stream_state`, and fixed failure/cleanup codes. `playing` means PLAY
was accepted; `stream_state: active` separately indicates media startup completed.
Two live tickets and 128 retained operation records bound allocations. Lists show
at most 16 owned records and report truncation. Identical open retries preserve the
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
