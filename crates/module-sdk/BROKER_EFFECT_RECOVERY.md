# Broker effect source slots and recovery

`EffectFencePolicy.source_slots` opts into the broker's recyclable source slots.
Without it the existing unslotted API and its 64-source lifetime bound remain
unchanged. Slot mode requires the host lifecycle control endpoint; legacy source
commands and legacy provider-retirement acknowledgments are rejected. Existing
API grants, provider selection, parent authority, and protected-route checks
still apply. Native plugins are trusted code, not sandboxed by these grants.

There are 64 deterministic slots. `EffectSource::slot()` maps an authenticated
lifecycle source ID to its slot; a collision with a live source fails closed.
A source has at most 32 uncompacted effect records. Slot tokens and broker epochs
are anti-replay identities, not credentials or evidence of authentication.

The selected authority queries `SelectedMethod` for a protected method and
validates its own semantic `schema_revision` against the returned versioned
provider identity and complete request/response API schema. `ArmSelected`
supplies that exact selection; the broker compares it with the currently
registered, issuer-callable selection under its admission lock. A changed
provider, interface or schema fails before a record is armed. Schema revisions
are authority policy, not inferred from the API schema. Slot mode rejects the
older unbound `Arm`; unslotted legacy callers retain their existing behavior.

## Broker startup and recovery

1. The host, selected issuer, each protected provider, and authority read
   `BrokerEpoch`. Before acknowledging `RecoveryRetired` with that epoch, each
   issuer/provider must actually retire its old workers and local transport
   obligations. The reserved host endpoint separately confirms that it joined
   the old workers. The broker denies recovery until all selected participants
   have confirmed. The authority must independently verify those retirements.
2. The selected authority reads its durable, caller-scoped State journal and
   reconciles any uncertain prior writes. It supplies the 64 durable retired slot
   incarnations to `RecoverSlots`, using the current broker epoch. A previous
   epoch cannot initialize the broker. Repeating an identical recovery request
   does not roll back later slot progress.
3. `SlotStatus { slot }` returns `{broker_epoch, ready:false,
   pending_retirements}` before recovery. Once ready, it returns
   `{broker_epoch, ready:true, lease, active}`, where `active` is null or the
   exact scope. The authority durably reserves the returned lease and source
   before `OpenSlot`. The slot token changes only after a successful close.
4. An unresolved journal entry need not contain request bytes. After the old
   workers are verified retired, use `OpenRecoverySlot` for its scope and the
   retained retired and issued sequence watermarks from the same snapshot. It is
   fenced from creation: it cannot arm or dispatch, and cannot close until every
   sequence through the issued watermark has been reconciled. Within its `SlotCommand`, use `RecoverFence` and `LookupIdentity`
   with `GuardedEffectIdentity`. The authority must match the source, provider,
   method, operation, digest and its separate cleanup identity to the durable
   journal. `retired` here does not reconstruct a provider result or prove that
   persistent effects were cleaned up. `retired_sequence` is only a sequence
   watermark, not an exact-effect receipt.

## Closing and reuse

`BeginCloseSlot` atomically prevents new admissions, fences recorded effects,
returns undelivered work to the broker's cancelled state and retains delivered
work until its selected provider sends `ProviderRetiredInEpoch`. A provider
captures `BrokerEpoch` before accepting work; an acknowledgment from another
broker epoch is rejected, even if its request ID is reused.

After close begins, the selected issuer and reserved host endpoint separately
send `SourceRetired` for the exact lease and scope, only after the host has joined
the corresponding issuer workers. The authority reconciles any durable journal
entries and actual effect/transport cleanup, confirms the retired effect prefix
with `Compact`, then durably persists the slot incarnation as retired before
`FinishCloseSlot`. A failed or uncertain State write, worker retirement, or close
reply keeps the source fenced until reconciled. Retry the same close or inspect
`SlotStatus`; never infer retirement from elapsed time. After close the token
rotates, so delayed opens and dispatches with the old lease fail. A restarted
broker also changes its epoch and starts closed.

Slot recovery depends on a correctly implemented selected authority, State
provider, issuer, provider and host. The broker cannot itself prove that a
native component actually joined its worker or cleaned an external effect. The
production host and pairing integration for these acknowledgments and durable
ordering are not supplied by this SDK increment; do not enable a production route
until that integration and restart tests pass.
