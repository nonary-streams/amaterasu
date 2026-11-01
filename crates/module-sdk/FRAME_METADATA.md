# Frame-associated metadata v1

This is an opt-in frame-side contract, not an HDR implementation. The selected
trusted composition chooses exactly one carriage owner (`EncoderEmbed` or
`FramerInsert`) per session and schema policy. Providers expose
`amaterasu.frame-metadata-capabilities.native/1` claims through
`FrameMetadataCapabilitiesApiV1`. `enumerate_capabilities` bounds and checks
those claims, while `MetadataCapabilitySnapshot::policy` combines them with
trusted owner/stage selection. A claim is not proof of implementation or a
native-code sandbox. The caller must track capability invalidation and supply
the current generation; a copied old generation is not evidence of freshness.

`FrameResourceMetadataFrameV1` wraps the unchanged frame with a separately
borrowed envelope. `CaptureApiV1_10`, `FrameResourceConsumerApiV1_1`, and
`FrameResourceProcessorApiV1_2` are additive tails; bind by typed family and
require both minor version and complete table size. Lack of a tail is not
permission to lose required metadata. Metadata bytes expire on synchronous
callback return even if the frame is retained. An asynchronous consumer must
copy the envelope separately and obey all original frame, acquire-fence,
completion-fence, provider-binding and cancellation contracts.

`copy_frame_metadata` checks the wrapper, exact authenticated session/source/
configuration identity, frame sequence and timestamp, understood clock domain
and nonzero epoch, and the frozen `MetadataPolicy`. Metadata does not authorize
its own identity. The caller supplies expected identity from trusted session
state and must separately validate the resource schema and frame lease. These
checks cannot prove a provider actually analyzed those pixels. A processor may
use `verify_processor_transition` to reject missing, altered or downgraded
forwarded metadata. The caller must invoke it; the ABI itself cannot prove it.

The one-in-flight `FrameMetadataAdmission` pins context and policy, owns bounded
copies of the original and policy-filtered envelope, rejects a different retry,
and requires increasing submission IDs once accepted or discarded. It checks
stable scalar frame/offer fields on retry. This is not a content hash or a frame
lease: the owning pipeline must independently retain or reacquire the same
immutable resource. `pending(generation)` and `prepare` reject a changed
capability generation. `retire` and `cancel` release this guard's bytes only;
provider work, frame leases and GPU fences still require their own barriers.
`PreparedFrameMetadata::encoder_options` borrows those bytes through a
lifetime-bound wrapper and uses `BUNDLE_SCHEMA` in `EncoderFrameOptionsV1`.

## AFM1 envelope

All integers are unsigned little-endian with no padding. The complete envelope
is at most 65536 bytes; there are at most 16 items and each nonempty item is at
most 16384 bytes. Items are strictly ordered by `(namespace_id, value, schema)`;
duplicates, unknown flag bits, invalid clocks, zero identity fields and trailing
bytes are rejected. Timestamp is in microseconds. Offset and field sizes:

| Offset | Field |
| --- | --- |
| 0 | `AFM1` magic, 4 bytes |
| 4 | carriage owner, u32: 1 encoder, 2 framer |
| 8 | session ID, u64 |
| 16 | submission ID, u64; equals frame sequence |
| 24 | source ID, u64 |
| 32 | source generation, u64 |
| 40 | configuration generation, u64 |
| 48 | frame timestamp, u64 |
| 56 | clock domain, u32: 1 stream-relative or 2 host monotonic |
| 60 | nonzero clock epoch, 16 bytes |
| 76 | item count, u32 |
| 80 | item records, concatenated |

Each item record has schema namespace/value/version (three u64), flags (u32;
bit 0 means required), payload length (u32) and exactly that many payload bytes.
Schema IDs are interpreted in the frame-metadata domain. Standard namespace 1,
version 1, values 1 and 2 identify respectively complete raw ITU-T T.35 HDR10+
and HDR Vivid payloads. They do not mean HEVC SEI or AV1 OBU bytes. Unknown
required schemas fail closed. Unknown optional schemas are dropped only when the
trusted policy explicitly permits it; a declared `FORWARD` capability preserves
the exact payload. No codec syntax is validated here.

The encoder options use side-data schema `{namespace_id: 1, value: 4, schema:
1}` and these exact AFM1 bytes. Under encoder ownership the encoder is the only
stage permitted to apply metadata; under framer ownership the encoder may only
forward it. The encoded-unit/framer owner must separately carry the submission
association and an explicit application state, and reject missing required data
or a second application. The submitted Video and Session paths carry the
bundle and support a selected framer owner. Production RTSP keeps required
metadata offers closed pending a complete managed-route replacement test.
Codec carriage, client support and real HDR hardware remain separate work.

`EncodedMetadataAssociation` is an opt-in SDK guard for this output-side work.
It makes a one-time claim on the pending encoder-stage admission and copies its
AFM1 bundle. A second claim, including after output application or transport
retirement, fails. If the association is lost, cancel or retire that submission
instead of rebuilding the admission and resetting its state. Keep one
`FrameMetadataAdmission` as the authoritative per-session/source epoch owner;
separate admission instances do not coordinate with one another. The guard checks the exact selected
unit and submission/source/configuration identities, preserves the original
source generation in that bundle, and requires one declared application owner
through encoder and framer stages before transport. Each selected capture,
processor and encoder provider must support the v1.2 generation lease. An
invalidation request stays pending while leases remain; an already pinned unit
may finish local admission before the provider completes invalidation. Video
v1.2 validates the association and invokes non-reentrant, nonblocking local
transport admission under one ownership barrier, retiring only after success.
Busy consumes nothing. Split v1.1 check/retire operations are insufficient to
authorize this path. Leases are logical claims, not locks held across event-loop
calls; teardown cancels admission before bounded unpin. The capture clock in AFM1 and
the separately selected stream clock in the encoded unit are distinct; the
guard does not infer a timestamp conversion. The application state is a
provider assertion, not proof of encoded bytes or client interoperability. The
caller must separately own the immutable access-unit bytes across retries; the
guard pins its scalar identity and length, not its contents.
The native unit and selected framer carry this state and bundle, but their
synthetic tests do not prove real codec carriage or client interoperability.
