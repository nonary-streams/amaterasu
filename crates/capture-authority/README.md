# Capture authority session retirement

The authority owns each privileged helper until the operating system confirms
that exact child has exited and has been reaped. Closing a frame socket,
requesting termination, completing a presentation thread, or reaching a timeout
is not that confirmation.

## Control protocol compatibility

The original `AMACA001` / `AMACAOK1` exchange remains available for existing
clients and returns one proxied frame descriptor. Its frame-close behavior is
unchanged. It does not provide a retirement receipt.

Clients requiring retirement proof first send the allocation-free `AMACAP02`
precheck and require `AMACP2OK`, with no descriptors. Both prechecks reverify the
current protected approval and immutable installation. There is no fallback to
the original open operation if this capability is unavailable.

`AMACA002` / `AMACAOK2` returns two descriptors, in order: the proxied frame
channel and an independent retirement receipt channel. Both are connected UNIX
sequenced-packet endpoints with root peers. The receipt channel is not the
frame channel and remains readable after frame I/O is interrupted.

For version two, the authority allocates no helper process or device until it
observes an exact first helper request, request ID 1 `HELLO`, and rechecks the
approval. The record remains queued for ordinary proxy validation. Clients must
register the receipt with their bounded lifecycle owner before sending HELLO.
A lost, malformed or timed-out open handoff therefore cannot leave an
unreceipted native helper. An idle or rejected handoff acknowledges retirement
without spawning a helper.

## Retirement outcome

The authority handles frame closure during both request admission and an
outstanding helper response. It closes the helper channel and retains the child
while requesting termination and polling for actual reap. Only after verified
reap and destruction of its helper owner does it send `AMACADN2`.

A genuine kill/wait syscall failure sends `AMACAER2`. The authority retains its
child and session thread and continues containment until actual reap; it never
later substitutes a success receipt for that failure. Interrupted syscalls are
retried without declaring failure. An exit racing kill is accepted only after a
positive reap result. `ECHILD` loses numeric PID ownership: the session remains
failed and retained, and no later signal or wait targets that reusable PID.
Timeouts never detach the child or count as success.

The unprivileged receipt API polls with `MSG_DONTWAIT` and `try_lock`, retains
pending descriptors, and caches exact success or terminal failure. EOF,
malformed data, unexpected descriptors and frame shutdown cannot establish
retirement. Normal Close combines the exact receipt with local presentation-thread
retirement and clears the cached frame; already admitted callbacks and retained
frame owners keep their rightful access until they release it. Global module
quiescence additionally waits for admitted callbacks and retained frame leases.

Legacy executable helpers use an owned local reaper created before spawning the
child. Its shared receipt owns the reaper JoinHandle; normal Drop only signals
cleanup when the lifecycle registry retains that receipt. Poll joins only a
finished reaper. Last-owner Drop joins as final containment, keeping native
ownership and provider code alive until retirement is proven. Retain the receipt
before greeting a helper, including probe and failed-setup paths.

## Validation boundary

Library tests use socketpairs, held gates and fake process reapers. They do not
launch privileged/native helpers or open capture devices. Validate both default
and `driver-read-leases` feature configurations; the retirement envelope does not
change frame protocol 1 or experimental frame protocol 4. This source validation
does not qualify an installed authority, driver or real client stream. The new
capture client requires an authority binary supporting the version-two handshake.

## Supervisor ownership

The privileged supervisor owns every session thread immediately after spawn,
including failed descriptor handoffs and unwinding. It signals all sessions
before joining any one, retains one bounded session-error outcome during service,
and joins every independent worker even after another result is an error or a
panic. Per-session failures remain confined while service continues; the final
supervisor result preserves the recorded failure. Session ownership is never
detached to make shutdown appear complete.

Authorized-worker retirement uses the same verified child reaper. The supervisor
reports real reap/termination failures while retaining the child and continues
containment until exit proof is available. If an earlier worker observation loses
child identity with ECHILD, it logs the unresolved failure and retains the owner
without signaling or waiting on the reusable numeric PID. This state cannot
return successful shutdown. These waits occur only in the dedicated privileged
supervisor, never the host's plugin tick.
