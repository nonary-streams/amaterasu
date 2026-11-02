# Orchestration ownership

`actions.rs` validates and authorizes public operations and owns their idempotency rules. `record.rs` retains caller identity, immutable prepare configuration, resolved endpoint metadata and component handles. `workflow.rs` defines the bounded child-operation sequence. `lifecycle.rs` advances preparation, activation and graceful draining; `cleanup.rs` owns rollback and release confirmation. `responses.rs` validates provider acknowledgements. `rpc.rs` retains broker request identity across transport failures. The native plane owns only bindings and copied media, never provider allocation lifetime.

Preparation creates a session-service record, opens the genuine CBR Opus encoder, derives the fixed audio wire length from its reported packet size (CBC PKCS7 adds a complete padding block when already aligned), then opens media and control transports. It returns bound ports with a bounded prepared deadline. Starting checks live authenticated control status first; an early request returns `control_not_authenticated` and retains the prepared allocation for retry. The caller must still meet the prepared deadline.

Media preparation accepts either the existing pair of fixed nonzero destination ports or a strict pair of distinct 16-character base64url discovery payloads with both ports omitted. The latter opens the UDP sockets before RTSP negotiation but does not authorize a destination from RTSP client-port placeholders. Media status retains only readiness and learned source ports; bearer payloads never enter public responses. After PLAY is armed, background work requires authenticated control and both healthy media lanes before acquiring leases or opening capture. It polls within the original prepared deadline, including when only one lane has arrived.

Activation acquires a shared provider/output lease, opens video with `packet_boundaries:true`, opens audio, validates 48 kHz stereo PCM and leases the resolved endpoint identity, then activates the session-service record and binds the native plane. The `default` audio alias is supported because its resolved endpoint, rather than the alias, supplies the SHA256 lease identifier. Leases coordinate read ownership; they do not represent exclusive display topology or input mutation authority.

Every allocation is marked as possibly committed before submitting its parented child request. A transport failure retains the exact broker operation and retries that same admission to discover its request ID. Cancellation and result release must be confirmed before caller-scoped lookup. Three failed settlement attempts expose `cleanup_failed`; explicit stop retries the retained uncertainty. No retry silently creates a new component operation identity. Healthy child results use the SDK ephemeral result path to release broker tracking.

Stop drains video, stops new audio capture, drains accepted PCM through Opus, waits for both EOS markers and the local UDP queue, then drops bindings and closes providers. Video close can acknowledge only the start of asynchronous cleanup, so lookup must confirm absence before session-service `stop` / `finish_stop` releases leases. Failed closes retain their handles and leases, and cleanup proceeds for independent remaining resources. Cleanup RPCs are parentless: ending the caller's stop request cannot cancel release work. A closed record preserves its prepare operation identity and never respawns on retry.

Media pumping continues independently of an in-flight control workflow. IDR requests coalesce; background control work alternates with ingress and rotates sessions so repeated requests cannot starve an explicit stop. Recognized input and feedback records are counted without OS effects. Malformed and wrong-direction records increment rejection counts. In particular, a host-to-client termination record received from a client is never interpreted as a stop command.

Synchronous module unload releases native bindings and makes a bounded attempt to cancel and release outstanding broker requests. It cannot tick other providers to confirm asynchronous teardown, drain network queues, or release session leases safely. Active allocations and leases remain until their providers unload; orderly stop must precede module replacement if confirmed cleanup is required. A runtime transport failure during unload can prevent cancellation confirmation. There is no persistent crash-recovery guarantee or seamless live replacement claim.

The pipeline retains at most two live or unresolved sessions and 128 lifetime records. Status lists return all live records first and at most 16 rows, with total/truncated metadata; direct ID and operation lookup remain available. Keys remain private in bounded in-memory configuration and child payloads, never in status or logs. Native modules remain trusted code.

The separate `management_live_sessions` read is restricted to the management
principal. It reports only active sessions, their requested video metadata,
optional client device name, monotonic and host Unix snapshot times, and exact decimal-string
host acceptance counters. It excludes transport addresses, keys and control
capabilities. Counters remain available independently of history recording;
they do not establish client delivery or playback.
An opaque per-process epoch lets readers discard counter baselines after a
module restart; it is not an authorization credential.
The Unix timestamp is a decimal millisecond string, allowing clients to compare
live observation intervals with retained samples without relying on browser clocks.

Tests use a stateful fake broker and native plane to exercise before/after-commit transport failure, lost admission and completion, parent cancellation, unconfirmed cancellation retry, late video-close confirmation, lease retention, resolved endpoint validation, auth-before-hardware retry, prepared expiry under ingress, control direction rejection, IDR fairness and unload limits. The separately signed synthetic integration composes the actual Opus, media UDP and ENet/GCM providers. Hardware capture, encoding and playback still require platform qualification.

## Starting after RTSP PLAY

The additive `start_when_authenticated {session, configuration}` action uses the same strict `StartConfig`, owner check and trusted caller allowlist as `start`. It returns the existing session metadata with `state: "armed"` immediately after accepting lifecycle ownership. Armed does not mean authenticated, active, or hardware opened. This matches the primary Moonlight connection order: the pinned client's `Connection.c` completes `performRtspHandshake()` (including PLAY) before calling `startControlStream()`.

Background work polls live control status at most once per 100 ms per armed session, alternating with ingress and rotating sessions. Waiting never extends the original prepare deadline. Once authentication is confirmed, state changes to `starting` and the existing startup workflow receives its own 15-second hardware deadline. Capture/encoder opens, shared leases, format validation, native binding, failure and rollback all use the existing implementation. Ordinary `start` still returns `control_not_authenticated` for an early request.

The initial arm checks that its caller request is active and requires confirmed completion before assuming ownership. Cancellation or uncertain completion before acceptance rolls back prepared resources. After a confirmed arm response, background child operations are parentless; cancelling a later status or duplicate arm request does not revoke the accepted lifecycle. Exact arm retries return its current state without opening components again; a changed configuration is rejected. Stop before authentication cancels the scheduled start and releases prepared resources. Failures remain visible through status/lookup, including cleanup uncertainty.

The action does not implement a network RTSP listener or claim that a PLAY response proves successful capture. A transport coordinator can acknowledge its scheduling decision and observe subsequent `armed`, `starting`, `active`, or failure states independently.


## Owned Linux displays

A PipeWire or external-provider start targeting `amaterasu-owned-display` allocates a unique monitor
through `amaterasu.virtual-display/1` after transport authentication and before
video capture. Its caller/UUID identity derives from the stable journaled stream
record. Monitor readiness must match requested timing, active state and the
returned named capture output. Failed startup cleans up the exact possibly
committed identity, including lost acknowledgements. The lifecycle journal stores
only the attempted flag and stable record identity, not a native lease capability.

Normal stop closes capture before removing that monitor and requires confirmed
absence before clearing the lifecycle journal. On restart, the display provider
also reconciles its own private ownership journal. A failed removal remains a
cleanup failure, not a successful stop. Only this stream's display is eligible.

The optional video v1.1 native tail forwards current caller-owned capture geometry.
It preserves the v1.0 packet prefix and works with older providers that have no
geometry. Logical Wayland desktop coordinates are kept separate from source image
pixels. The pointer is centered on the owned output after an authorized mouse
lease is acquired; later topology updates refresh absolute pointer mapping before
control packets are translated. Geometry is not inferred from client resolution.

## Interrupted external-capture cleanup

A restart first reconciles every recorded native resource using its original
operation identity. For an external-capture lifecycle marker, a delegated capture
consumer then sends `retire_stream` with `restart_recovery: true`, the old stream
target, and confirmed native retirement evidence. The coordinator accepts only an
exact old epoch/generation in recovery and runs its frozen provider release graph;
it never replays prepare. The stream journal clears only after that cleanup is
confirmed. Unknown outcomes and managed-capture markers remain fenced.

## Cooperative host shutdown

The optional SDK `amaterasu_module_quiesce_v1` contract keeps all providers ticking
while consumers drain their owned work. NVHTTP stops admitting client work and
runs stream, application, and lifecycle cleanup before reporting ready. The host
bounds this drain to twenty seconds before reverse-order destruction; its worker
supervisor allows twenty-five seconds. Timeout paths retain recovery evidence.
