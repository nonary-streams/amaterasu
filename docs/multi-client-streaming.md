# Shared application streaming

The lifecycle path admits two Moonlight clients to one running application.
`currentgame` is visible to trusted clients with launch permission, so Moonlight
selects `/resume` for the running game. A fresh `/launch` for that same application
also joins it. Each client has independent encryption keys, RTSP identity, media
sockets, input binding, feedback and history activity. The application is launched
once. Identical request retries reuse their existing ticket.

The retained application anchor owns process termination and the lifecycle
session. A joining transport borrows that application; it cannot stop the process
as part of disconnect, lost reply, revocation or failed admission. Explicit Quit
App from a member and confirmed application exit end the group. Transport cleanup
must settle before the application owner releases shared resources. Steam games
follow the same disconnect/resume lifetime as other applications.

## Provider contracts

`StreamIntent::Join` requires the explicit `amaterasu.stream/join` startup grant
for every selected participant. Providers authenticate their normal coordinator
request and use the same public SDK Join adapter available to replacements.
Unaware providers reject Join; consumers do not substitute a default provider.
The coordinator derives `StreamJoinContext` from the existing source transition.
A client cannot supply resource identity or grant native mutation rights.
Existing operator compositions need the same explicit Join grants and the
capture consumer's `resolve_stream_preparation` operation grant as newly
generated profiles. Replacing library bytes alone does not grant either right.

Join preserves the original source geometry and host effects, including display,
frame-limiter and HDR policy. A joined client retains its own input/media request
and authenticated identity. Capture/output transformations still require support
from the selected capture, processing and encoder providers; protocol admission
does not establish a working transform for every frame type.

An exact stream consumer retirement can retain the original source lease.
`StreamRetirementProof` distinguishes `consumer_retired` from
`resources_released`, and binds shared retention to an exact source stream,
epoch, session and generation. RTSP and stream-session preserve that distinction
and still require their own media/input/capture teardown. Earlier departures can
complete with `shared_retained`; final resource cleanup must complete before a
full-release receipt succeeds. Restart recovery must retain the same ownership
and uncertainty fences.

An external capture source owns one SDK virtual-display reference for its
subscribers. The coordinator's read-only `resolve_stream_preparation` operation
recovers an exact admission when a crash occurs before the local stream target
is recorded. It requires an explicit startup operation grant and checks the
session, original caller and preparation operation. Pending or invalid replies
retain the display obligation; only authenticated definitive absence can clear
an admission without a target.

## Linux input and transport

Linux lifecycle-bound input contexts own separate device state and companion
worker grants. Exact generation/token cleanup cannot revoke a sibling that shares
the broker process. Global policy updates reach every context, while scoped
settings, held input, controller slots and feedback remain independent. Legacy
unbound input and Windows input retain exclusive behavior.

The first transport reserves configured UDP endpoints at base +9 through +11.
The second transport on that admitted local address uses base +12 through +14
through the selected Network provider. RTSP advertises the actual provider
ports. Remote access requires firewall rules for both triplets. Pending admission
and uncertain teardown retain their reservations; a third admission fails until
a slot is available, and socket reuse does not route client traffic.

Named PipeWire CPU frames can be resized and letterboxed independently for each
client. Video and absolute input share the exact fitted rectangle, including
uneven pixel borders. This does not enable resizing for other frame types.

Settled joining-record metadata is bounded and can be retired. Exact SHA-256 key
fingerprints preserve replay rejection after eviction; exhausting the bounded
key history rejects new admissions rather than forgetting replay fences.

## Qualification

Unit and broker tests cover two clients, one application launch, distinct tickets
and keys, both disconnect orders, revocation, explicit group quit, lost replies,
rejected status/history, source-retirement evidence and bounded reconnect churn.
Network tests use real loopback SDK sockets, including IPv6. Input qualification
uses an isolated backend without desktop effects. Independently built provider
tests exercise the public SDK contracts.

These checks do not prove two real Moonlight clients simultaneously rendering,
audible audio, physical controller feedback, or installed-host behavior. Those
remain separate runtime qualification gates.
