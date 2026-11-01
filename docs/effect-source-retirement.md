# Pairing source retirement and protocol restart

These additive operations use `amaterasu.pairing-effects/1` and `amaterasu_module_sdk::effect_source_retirement`. They are available only when a trusted startup composition explicitly selects the protocol and effect owners. Each request must be a direct broker call: caller and initiator must be the same selected principal. This source change does not alter installed grants. Native components are trusted code; API grants are not a sandbox.

A source is the complete SDK `effect_authority::Source`, including host/session/stream, source and device generations, and effect-owner epoch. Its `StreamScope` contains the exact feedback capabilities, controller slot, selected control service, stream handle and provider epoch. These fields are not by themselves proof of negotiation or transport ownership.

## Source fence and retirement

* `fence_source { epoch, source, scope, operation_id }` is accepted from the selected direct protocol or effect owner. It synchronously denies new binds and lease admissions for that exact source. It also fences an as-yet-unbound source so a delayed bind cannot resurrect it. A retry of the exact tuple is idempotent; a changed operation ID or scope conflicts. This fence is source-specific and does not retire other streams.
* `complete_source_retirement { epoch, source, scope, operation_id, retirement }` is accepted only from the selected direct effect owner. The owner must first close its local producer, retire the exact whole control stream and its local queues, complete the pairing stream lease with that transport receipt, and acknowledge the completed lease. If a lease was ever admitted, `retirement` must be the identical receipt used for its completion; otherwise it may be null. Pairing checks receipt fields and retains the source fence. The receipt is the trusted owner's assertion that it verified the selected provider's broker reply, not cryptographic proof or remote delivery.
* `lookup_source_retirement { epoch, source, scope, operation_id }` is available to either selected owner for lost-reply reconciliation. Source-specific operations return `effect_source_retirement::Receipt`: `{ ok, epoch, source, scope, operation_id, state }`, with state `fenced` or `retired`. Consumers must compare every field and treat `fenced` as cleanup only.
* `lookup_current_source_retirement { epoch, source, scope }` lets either owner discover the existing retirement operation ID when the other owner fenced the same source first. It returns the same exact receipt and never creates a fence.

Tombstones prevent delayed binds until a drained authority epoch is rotated. They are bounded at 4,096; exhaustion fails closed and requires a coordinated global drain and epoch rotation. Completed source records release the 64 live-source slots, but neither a timeout nor a lost response removes a tombstone. The selected input/stream owner must also observe or initiate the source fence, stop local admission, and wait for the actual control-provider receipt; a source-level fence alone does not synchronously notify its worker. Already handed-off packets cannot be recalled.

Whole-control-stream retirement can affect several controller sources. Pairing will not mark any one source retired while another bound source uses the same service, stream handle and provider epoch, or while any lease on that transport remains in flight or unacknowledged. The owner must fence each affected source and settle all those leases with the same exact transport receipt. That retired transport incarnation cannot be rebound in the current authority epoch; a replacement needs a new transport handle or provider epoch. Sources on a different transport are not fenced by the source operation.

Within the same lifecycle host epoch and session, a replacement for a controller slot cannot bind while the previous slot source is live or awaiting retirement, even when the stream generation changes. Lifecycle host-epoch replacement still needs its own coordinated fence and owner retirement; a newly minted epoch alone is not that proof.

For an uncertain NVHTTP bind, the adapter uses `BindFeedbackSource::retirement_operation_id()` to fence the exact source. The ID is `nvhttp-source-` plus the lowercase SHA-256 of the domain `amaterasu.nvhttp.source-retirement/v1` followed by a zero byte and the complete `Source` in declared field order: `version` as one byte; each string as its UTF-8 byte length in an unsigned 64-bit big-endian integer followed by its bytes; and each generation as an unsigned 64-bit big-endian integer. It refuses new joins while the source is unresolved. The selected effect owner can retire the source and call NVHTTP `reconcile_feedback_source` with the original bind fields; NVHTTP clears the in-process recovery state only after the pairing retirement receipt says `retired`. If pairing restarted and the old epoch is gone, NVHTTP accepts only a new, unfenced, empty pairing epoch whose selected effect owner has completed startup recovery. These checks rely on the selected effect owner's documented worker-retirement assertion.

## Protocol restart

On startup with the optional source join selected, NVHTTP obtains the pairing authority epoch and submits `fence_protocol_restart { epoch, operation_id }` as the direct protocol owner. The reply is `{ ok, epoch, operation_id, state: "fenced", fence_generation }`. Pairing synchronously fences the old epoch, clears live source bindings, and retains outstanding effects for owner retirement. The selected effect owner must retire all affected workers and local transports, complete outstanding admissions and acknowledge the global drain; it then calls pairing `recover`, which rotates the authority epoch. NVHTTP accepts new joins only after observing a different, unfenced, empty epoch with startup recovery complete. A missing provider, uncertain response, stale epoch, pending retirement, or live source fails closed. This conservative restart fence covers all streams of the selected protocol owner and can require their retirement; it does not silently claim selective preservation.

Production NVHTTP and built-in pairing source-binding gates remain closed. The selected stream owner still needs to produce the validated source/device/slot/transport/negotiated scope, connect its final-send and retirement path, and pass combined real-provider race and restart tests. Host-configuration trust changes also require the separate transaction fence.

## Feedback effect-owner restart recovery v1

The selected direct effect owner can synchronously fence its prior live Pairing
epoch and capture a bounded immutable recovery snapshot. This is a separate
feedback contract in `effect_owner_recovery`; it does not authorize or retire
generic protocol effects. Both broker caller and initiator must equal the
startup-selected effect owner. No certificate or client identity is returned.

1. Read pairing `status`. If it reports an extant `owner_epoch` and no pairing
   provider startup recovery, submit `fence_effect_owner_restart` with
   `{version:1,epoch,owner_epoch,operation_id}`. The old owner epoch is the
   exact epoch reported by Pairing, not a new owner-generated guess. The result
   is `{ok,version,epoch,owner_epoch,operation_id,fence_generation,count}`.
   Retry the same tuple after a lost reply; a different restart operation is
   rejected while this snapshot exists. `count` is at most 4160.
2. Read each zero-based index below `count` with `read_effect_owner_restart`
   and the same version, epoch, owner epoch and operation ID. Each page returns
   `{ok,version,epoch,owner_epoch,operation_id,index,count,record}`. A record
   contains the exact `source`, `scope`, optional `lease` and optional
   `source_retirement` (its `operation_id`, completion flag and retained
   receipt). It includes already retired tombstones. `transport_retirement`
   retains an admitted lease's receipt after its ticket was acknowledged. A
   lease has its `operation_id`, `sequence`,
   ticket, completion flag and, when already completed, the exact retirement
   assertion retained by Pairing. Fields are required even when null. Pages
   remain unchanged as settlement proceeds and are discarded only on a
   successful authority epoch rotation. Validate the fence and every page with
   the SDK matchers, including count and index; missing pages fail closed.
3. Stop all old producers. Group the records by full transport service, stream
   handle and provider epoch, fence every source, and reconcile the actual
   selected control provider's retirement for each group. A saved Pairing
   assertion is an operation identity for reconciling that provider, never a
   substitute for its fresh verified broker reply. Use the exact retained
   receipt for a lease already completed; complete each other lease using the
   same actual provider reply, acknowledge every ticket, then complete each
   source retirement. Submit `confirm_effect_owner_restart` for each index,
   with the same version, epochs and operation ID plus `index` and the verified
   `retirement`. Confirmations are idempotent and must agree across the same
   transport, including live lease and source completions regardless of their
   ordering. Even a source with no lease or an already retired source requires
   a matching whole-control-stream receipt in this restart flow. Pairing checks the
   selected owner's assertion and matching fields; it cannot independently
   authenticate that the owner observed the provider response.
4. Read the current fence generation from `status`, acknowledge `drain` only
   after all pages and providers have settled, and then use `recover` to rotate
   to the new owner epoch. A concurrent configuration hold may advance the
   generation; the snapshot remains valid, but drain must use the latest
   generation. No timeout or lost reply proves retirement.

If Pairing itself restarted, its volatile ledger and old owner epoch are gone;
this API refuses to invent an empty snapshot. The existing pairing-provider
startup recovery requires separate proof that all old owner transports retired.
Likewise, unscoped effects cannot be enumerated here and cause this restart
attempt to fail closed; resolving them belongs to their own authority. This
The stream owner has a qualification-only startup consumer that fences and
enumerates the old owner, reconciles each selected control-provider receipt,
settles leases and sources, and verifies the new unfenced authority epoch. Its
broker/real-control tests use fixture-seeded pairing source state; the full
certificate, NVHTTP, lifecycle, input and stream join and process-restart proof
remain outstanding. Production feedback admission and this startup consumer
remain closed pending combined proof and independent review.

## Host-configuration transaction holds

When trusted startup composition selects the direct `configuration_owner` and
enables the corresponding host-configuration authority, a v1 configuration
change first persists its caller-bound transaction marker and begins the
pairing hold. The owner must fence every scoped source captured by that global
fence, including every source sharing a control transport. It must complete and
acknowledge each lease, complete each source retirement with the same verified
whole-stream receipt, and finally acknowledge the current global fence
generation. The configuration hold remains pending until all captured sources
are retired and the selected owner acknowledges the drain. A source without an
admitted lease still needs its source-retirement completion; an empty ledger
still needs the selected owner's drain acknowledgement.

The hold remains fenced through State commit and validated identity reload, or
until reconciliation has advanced the same State namespace revision to prove
an abort. Pairing restart checks the host-configuration marker before reopening
admission. If the owner recovers an epoch while a marker is pending, pairing
keeps the new epoch fenced and uses reconciliation to settle the original
operation. Host configuration exposes bounded direct status reads while its
update is pending, so this verification does not wait behind the held update.
This barrier covers the selected feedback leases and source retirements; it
does not prove retirement of arbitrary protocol effects.

## Generic protocol effects

The source tombstone currently covers feedback stream leases only. Its `StreamScope` describes client-bound controller feedback, and pairing still denies the old unscoped `admit` operation with `scope_required`. A protocol journal must not treat this receipt alone as evidence that arbitrary custom protocol effects have retired. In particular, `protocol_authority::EffectOperation::admit` currently sends that denied unscoped operation. Reusing this authority for those effects requires an additive purpose-specific scope, a trusted mapping from the exact effect to its source and issuer incarnation, and selected per-capability client policy. The runtime-owned primitive must serialize that authority and its durable exact-effect fence with the actual broker `CallApi` admission, then retain and drain every tracked provider effect before journal compaction. Pairing's asynchronous source-fence request followed by a separate `CallApi` is not that primitive. Pairing's volatile source tombstones also do not replace the protocol journal's durable fence after restart. No generic protocol ingress is enabled by these feedback operations.
