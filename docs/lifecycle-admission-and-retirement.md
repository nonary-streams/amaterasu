# Admission and retirement

A stream that has ended must not decide whether, or how soon, the next one
starts. This document states the rule that enforces that, where each layer
applies it, and what is not yet covered.

## The rule

Every lifecycle layer keeps records of *owners*: a launch, a transport ticket,
a stream session, a lifecycle session. An owner has two independent facts.

| Fact | Values | Meaning |
| --- | --- | --- |
| Admission | open, ended | Whether the owner may still create or change live resources. |
| Retirement | confirmed, superseded, abandoned, unconfirmed, pending | What the owner's cleanup still owes after it ended. |

Ending is a decision. It needs no provider's cooperation and it is not undone.
Admission of new work reads the first fact and never the second:

- A limit on concurrent owners counts open owners.
- A full record table makes room by dropping ended owners, the most settled
  first. It refuses a new owner only when it is full of open ones.
- An exclusive resource held by an open owner is a real conflict. One held by
  an ended owner is waited for a bounded time and then taken over, or reported
  as that one unavailable resource when it cannot be taken over.
- Cleanup of an ended owner runs beside new work and beside other cleanups. It
  does not occupy the slot that reads requests.

Unconfirmed cleanup is never reported as confirmed. A record that is dropped,
an entry that is forgotten and a domain that is taken over are each logged
with the owner they belonged to.

## SDK support

`amaterasu_module_sdk::lifecycle_ledger` holds the part every layer shares.

- `Owner` is implemented once per record type and maps the layer's own states
  onto the two facts.
- `Live` is the only view of the records an admission decision receives. The
  module builds it and it contains open owners only, so the decision cannot
  depend on an ended owner. Each layer's decision is a function that takes a
  `Live` and no other access to the records.
- `make_room` drops ended owners from a bounded map, the most settled first.
  The layer says which records are pinned and receives each dropped record.

A property test in that module checks that adding any number of ended owners,
in any retirement state, changes no admission decision.

## Where each layer applies it

| Layer | Open while | Change |
| --- | --- | --- |
| NVHTTP launch | Starting, delivered, ready or paused. A root retiring only its own transport keeps its application and stays open. | The application slot is decided from open records. A launch waits at most 15 seconds past the previous application's end, then starts beside its cleanup. Each ended record has its own job. Quit and the console act on the application that is still held. |
| Launch | Starting or running. | Each stopping launch has its own cleanup workflow; requests are no longer answered `workflow_busy` while one runs. The 32 and 16-per-caller limits count open launches. |
| RTSP | From admission until asked to close. | The ticket limit counts open tickets. A port slot whose ticket ended without confirming is reused when none is free, oldest end first. Each ended ticket has its own job. Startup reconciliation skips this runtime's sessions and is waited for at most ten seconds. |
| Stream session | From preparation until asked to stop. | The session limit counts open sessions. Records are dropped when the table fills; before this, no record was ever removed. A full durable journal forgets entries of ended sessions. |
| Lifecycle coordinator | Session: preparing, ready or paused. Stream: preparing, ready or active. | A mutation domain held by an ended owner can be taken over. Admissions of an earlier host run do not use the admission bound. |

### Bounded waits

A successor still waits briefly for an ended predecessor, because normal
cleanup finishes in that time and an orderly handover is better than a forced
one. Each wait has a fixed end.

| Wait | Limit | Then |
| --- | --- | --- |
| NVHTTP launch behind an ended application | 15 s from that application's end | The launch starts beside the cleanup. |
| RTSP open behind startup reconciliation | 10 s from module start, or until the pass completes or blocks | The ticket is admitted. |
| Coordinator plan behind an ended holder of this host run | 10 s | The domain is taken over if the holder's provider was granted takeover. |
| Coordinator plan behind any holder | 20 s | The step fails and names the domain and holder. |

A holder left by an earlier host run is not waited for at all.

### Domain takeover in the coordinator

An admission holds its mutation domains until its provider confirms the work
it admitted. The coordinator's journal says which holders have ended:

- An admission this process did not make was made by an earlier host run.
  Nothing of that run is still executing. This is decided per admission, not
  per session, because releasing a leftover session makes new admissions.
- An admission of this run has ended when its session is past `paused`, or
  its stream is past `active` or gone.

A successor's plan takes such a holder's domains over, at once for an earlier
run and after ten seconds for this run. In-run takeover additionally requires
that the holder's provider was granted
`amaterasu.lifecycle/domain-takeover` by trusted composition. The grant means
the provider stops changing a domain for an earlier admission once a request
arrives under a newer one, and reports the earlier work's outcome as it
actually ended.

The superseded admission stays on record with its plan and cleanup
obligations. It holds no domain, counts against no bound and can dispatch
nothing: the rights check before every callback refuses an admission that
lost its domains. A late outcome closes its own record and can release,
refresh or publish nothing of its successor.

## What is given up, and when

These happen only when a bound is reached, and each is logged.

- **NVHTTP, RTSP, launch, stream session:** an ended record is dropped from
  the in-memory table. Retained broker calls are settled or handed to
  settlement first. A launch record dropped while its stop was unconfirmed is
  no longer tracked by the launch module.
- **Stream session journal:** at 32 entries, the entry of an ended session is
  forgotten to admit a new one. That session's cleanup is then not recovered
  after a restart. An open session's entry is never forgotten.

## Not covered yet

- **Stream session cleanup shares one job slot with new requests.** A session
  stop that takes long delays reading the next request in that module, up to
  the stop's own deadline. Running them side by side needs journal writes to
  be ordered first, because each job derives a whole journal document.
- **The coordinator's session table still counts ended sessions.** It holds
  128 sessions and is validated at that size on load. Counting open sessions
  only needs a way to drop an unretired session with its proofs.
- **No provider is granted domain takeover.** In-run takeover is therefore
  off. The frame limiter plans every session against one exclusive domain and
  is the first provider to review for the grant.
- **A reused RTSP port slot is not fenced downstream.** If the earlier
  session's sockets are still bound, the new session fails to bind and
  reports that. The transport provider would need to close the earlier
  holder's sockets when a newer ticket asks for the same address.
- **Replay fences still refuse when full.** The launch key and operation
  fences are bounded and are not evicted, since evicting one would allow a
  replay.
