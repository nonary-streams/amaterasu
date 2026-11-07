# Input companion worker clients

The Linux Unix socket client and Windows named pipe client use the existing v1
worker wire frames. Neither `InputPacket::version`, the endpoint names, service
permission policy nor the grant schema needs an upgrade for acknowledgement
ordering. Windows grants may omit the Linux-only `gamepad` capability.

Successful v1 service replies stay in request order and are not duplicated,
with an uncorrelated `ok` for both Event and Close. Permit expiry may instead
send a terminal `denied`, which never certifies cleanup. Clients therefore
retain exactly one outstanding operation. A timed-out Event or feedback poll must consume its own reply before
Close is written. A timed-out Close retains that same request; a retry observes
its acknowledgement without writing another Close. A rejected input operation
still permits an explicit Close to release its devices.

Only the acknowledgement of that Close proves cleanup to the client. Writing a
Close frame or dropping the transport requests service-owned revocation but
cannot be reported as completed native cleanup. Ambiguous writes, malformed
replies, unexpected reply kinds and terminal transport errors fence the client;
a later bare `ok` cannot clear that uncertainty. New input is closed as soon as
Close is requested, including while an earlier reply is still pending.

`restrict_deadline(Instant)` stores the earliest absolute shutdown deadline
without sending input or beginning cleanup. `connect_with_deadline` applies the
same limit to the initial Start exchange. Expired deadlines admit no new frame,
consume no acknowledgement and cannot be extended by a later announcement.
Settling an earlier reply and sending Close share one per-call reply budget.
The Unix client uses nonblocking connect/read/write and polls only the remaining
absolute budget; partial reply bytes survive observation timeouts.

Windows clients retain the existing v1 data-only access and server DACL. Their
overlapped operations own the buffer, event and `OVERLAPPED` allocation until
actual terminal completion. A timeout requests cancellation and fences new I/O
while the operation is pending; cancellation alone is not retirement. Forced
Drop waits for completion to preserve memory safety, so a stalled kernel
cancellation can exceed the cooperative budget. Containment of the outer worker
process remains the final fallback. Native Windows completion and driver
retirement still require Windows qualification.

Synthetic Unix and transport-independent tests cover delayed Event replies,
rejected Close, fragmented reply recovery, Close retry ownership, ambiguous
writes, expired initial handshakes and native socket backpressure. These tests
do not operate uinput devices or qualify native Windows driver retirement.
