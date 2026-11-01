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

`EffectFencePolicy.require_handoff` opts a slotted route into the atomic pairing
request handoff. It requires startup selection of `amaterasu.pairing-effects/1`
for the authority and grants for the issuer. In this mode `ArmSelected` is
denied: `CommitAdmission`, within `SlotCommand`, verifies a fresh exact canonical
journal entry, the selected provider and schema, and the live delivered v2
pairing request including its source, payload digest, and cleanup reference.
It arms the effect and completes that same request under the broker lock. A
cancellation that wins first prevents the arm; cancellation after commit returns
`effect_admission_uncertain` and requires explicit fencing and retirement.
Releasing an RPC result never retires its effect. This is an opt-in contract;
the pairing actor and production source route still need to invoke it.

## Broker startup and recovery

1. The host, selected issuer, each protected provider, explicitly selected State
   provider, and authority read
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
   sequence through the issued watermark has been reconciled. Legacy
   `RecoverFence` cannot create historical records in a recovery-only slot;
   use a verified host retirement record and `HostRecoverHistorical` or
   `HostRecoverHistoricalInstance`. The authority must match the source, provider,
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

`VerifyHistoricalJournal`, inside a recovery-only `SlotCommand`, verifies a
retained authority-owned State Get from the explicitly selected State provider
against `pairing/protocol-slot/v1/{slot:02}`. It requires the saved original
effect, selected method, provider incarnation and cleanup identity to match;
the persisted recovery claim must also contain the broker's current exact lease.
It denies stale snapshots, pending State writes and uncertain delivered writes.
The result only confirms journal data; it cannot fence, retire, dispatch, or
prove cleanup. Entries lacking an incarnation fail closed. The authority must
select a State implementation with durable atomic CAS and preserve its caller
namespace. Historical provider retirement and persistent cleanup still require
independent proofs; a newly selected provider cannot supply them.

The host may issue `HostProviderJoined` only after it independently proves that
the exact provider incarnation's workers have joined. The broker then stops
admissions to that instance and can return a `HostRetirementReceipt` for an exact
fenced effect. After restart, the trusted host may supply that old receipt to
`HostRecoverHistorical` together with the retained authenticated pairing-journal
read. The broker checks the old broker epoch against the entry's `issued_lease`,
the full entry, provider incarnation and current recovery lease. This establishes
only local worker retirement, never persistent cleanup. `Compact` stays blocked
until `ConfirmHistoricalCleanup` verifies a fresh journal entry with pairing's
durable `cleanup_confirmed` flag. Pairing must set that flag only after its own
independent cleanup proof; the broker does not perform that cleanup.

The host currently generates 128-bit random incarnation IDs and rejects
duplicates within a load. Their cross-restart uniqueness is probabilistic; the
format does not claim a deterministic, durable uniqueness guarantee.

The host reads the joined provider's stable broker snapshot one receipt at a
time and CAS-writes each exact receipt in its State namespace at
`host/effect-retirement/v2/{slot:02}/{slot-incarnation}/{sequence}`. A retry
accepts an identical saved receipt and fails on a conflicting value. The broker
continues to read the legacy v1 slot arrays during recovery. After restart,
`HostRecoverHistorical` requires a retained host-namespace State Get containing
the exact old receipt in addition to the pairing-journal read. The host must
preserve unresolved records until its lifecycle and pairing cleanup complete.
`AuthorizeHostReceiptCleanup` permits the host to delete one exact v2 receipt
only after fresh selected-State reads show both that receipt and the authority's
retired slot floor, and the broker has closed the same slot incarnation. The
result permits one revision-checked deletion; it is not a cleanup receipt for an
external effect. The pairing/host coordination that invokes this command is not
yet wired. State capacity exhaustion remains fail-closed and the host must not
evict by age.

On shutdown, the host first fences a protected provider and polls its optional
`amaterasu_module_retirement_v1` capability. Only an explicit successful result
before the deadline permits `HostProviderJoined` and receipt persistence. An
absent capability, failure, panic or timeout leaves recovery fenced. The poll
must be bounded and nonblocking; native modules are trusted to honor that ABI.
The host also persists the broker-issued `HostProviderInstanceRetirement` at
`host/provider-retirement/v1/{broker-epoch}/{provider-incarnation}` after that
join. `HostRecoverHistoricalInstance` accepts it with a fresh canonical journal
read for the exact old provider and incarnation, even when the old broker never
retained an effect record. Treat that entry as possibly executed; this receipt
does not establish issuer retirement or external cleanup. Old hosts that did not
write the record remain closed. Incarnation records currently require a future
coordinated cleanup mechanism and remain bounded by State capacity.
The generic module destroy call alone is not proof because the SDK catches
destructor panics without reporting whether joins completed. Startup pairing
replay and external cleanup remain separate integration work. A missing receipt
or cleanup proof leaves the historical source fenced.
