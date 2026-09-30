# Native media plane

The pipeline opens and closes all provider resources under the stream-session
principal. `Plane` retains five SDK native bindings and copies borrowed callback
output before returning. Callbacks never invoke another provider. No pointer into
provider storage survives its callback, and partial binding failure releases all
bindings acquired so far.

`tick` reads up to four authenticated control records first. Closed or failed
control delivery stops media forwarding. Initial authentication is a pipeline
precondition; an empty authenticated control queue is normal. Records preserve the
message kind, authenticated sequence and bounded body for the pipeline's typed
parser. Channel and delivery metadata confer no authority. Unknown messages are
not represented as executed actions.

Each video tick reads at most four fragments. The assembler accepts only one
sequence and timestamp until `FLAG_END_OF_ACCESS_UNIT`; it combines keyframe flags
and forwards a complete access unit immediately. A different sequence, different
timestamp, or EOS before the marker fails visibly. It never guesses boundaries
from later frames. One assembly is capped at 8 MiB and 1,024 fragments; a copied
incoming fragment can temporarily add another 8 MiB. The encoder must declare a
real access-unit boundary through the ABI flag.

Host feedback enters through `Plane::enqueue_feedback`, which accepts an owned,
validated `HostFeedback` into a session-bound queue capped at 64 messages and
16 KiB. The result `Queued` means the plane now owns the event; it is not transport
admission or client acknowledgement. When full, the plane may drop best-effort
PCM or evict an older PCM event to preserve a reliable rumble stop, trigger,
motion-state, or LED update. It never drops a reliable event to admit PCM; if no
PCM can be evicted, the caller gets `QueueFull` and must retain the source event.
PCM sequence gaps therefore indicate bounded best-effort loss.

After each authenticated control receive pass, the plane attempts up to four
feedback sends through the control handle already bound to this stream. The
provider encrypts the typed inner record with its own sequence and sends it only
to the authorized peer. `STATUS_OK` removes the event from the plane queue because
the provider copied it into its own bounded outgoing queue; this does not mean the
client received or consumed it. `MEDIA_BUSY` and `MEDIA_PENDING` consume nothing
and retain the exact event at the queue head for retry. `stop` discards pending
feedback and prevents later enqueue attempts from reviving it.

PCM records require both the negotiated control feature and capability for the
specific client-relative controller slot. The input/session coordinator must
apply those gates before enqueueing; a structurally valid codec record alone does
not prove the client advertised haptics support.

Audio requires 48 kHz stereo interleaved f32 or i16 PCM. No rate conversion or
channel remix is invented. At most one PCM frame and one encoded Opus packet are
retained by the plane, each bounded by the media ABI. Up to four encoded packets
are read per tick. Busy transport output stops further polling of that media path;
busy PCM submission retains the same input and stops capture reads while encoder
output can still be drained. All retries preserve bytes and metadata.

`stop` disables new PCM capture. Previously accepted pending PCM is submitted
before encoder drain. The pipeline separately requests video drain and may close
capture after stopping the plane. Both provider EOS markers must be accepted by
the media transport before the plane calls transport drain. `is_drained` means
that drain was accepted, not that UDP packets were delivered: the pipeline still
checks transport queue status before dropping the plane and closing providers.
Failures are sticky and require pipeline cleanup.

Tests use fake backends and actual C interface tables through a mock SDK runtime.
They cover immediate fragment assembly, missing boundaries, memory/count bounds,
blocked output, retained PCM on stop, EOS ordering, closed control gating,
callback byte ownership, duplicate callbacks, inconsistent callback status, and
complete/partial binding release. They do not qualify capture hardware or a real
Moonlight streaming session.
