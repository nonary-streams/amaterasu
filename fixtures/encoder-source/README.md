# Independent encoder provider fixture

`amaterasu-encoder-source` is a trusted native test module providing control and
native v1.0 interfaces as `example.encode/1`. It demonstrates an independently
built provider; it requires empty settings. Configure video with
`external_encoder_providers: {"vendor":"example.encode/1"}` and select
`encoder_provider: "vendor"` on open. The host package must grant video a startup
dependency on the fixture and both call and native-use rights for that service.

The fixture advertises CPU input and H.264, but produces a deterministic **fake,
non-decodable payload**, not an H.264 bitstream. It is suitable only for testing
control routing, ownership, backpressure, and packet lifecycle. It does not prove
stream playback, encoding quality, DMA-BUF, or real source capture. It accepts
at most two pending packets per session, retains rejected output, and delivers one
EOS after drain. Sessions and operation records are bounded. Native bindings and
control sessions are scoped to their authenticated caller.

The separate `example.encode-provider/1` native service demonstrates source mode.
It shares the control service's caller-scoped session IDs: open with the existing
control protocol with `mode:"source"` and a nonzero `source_id`, then configure
that ID with `ENCODER_SESSION_SOURCE`, the matching source ID, a nonzero
generation, standard H.264 and Annex B identifiers, and
`ENCODER_SCHEMA_COMMON_CONFIG` options containing validated 8-bit 4:2:0 geometry,
frame rate and bitrate matching the authorized open request. The host must also
grant native use of this service. It produces one
marked fake access unit at timestamp 1000 microseconds without submitting a frame,
then returns pending until drain. It supports synchronous ready/stopped
notifications, idempotent unsubscribe and destroy, and a bounded store of
idempotent control responses (at most 128 records and 128 KiB, including
reserved per-record overhead). Its probe advertises these limits with the standard `EncoderProviderLimitsV1`
schema. Reconfigure with empty data advances the generation;
queued output retains its original generation. State requests with empty data
are acknowledged; keyframe, recovery, other payloads and operations are
unsupported. This synthetic provider is not a real capture or encoder backend.

The separate `example.encode-consumer/1` service accepts synchronous BGRA8 and
NV12 CPU resources into the legacy output queue. Generic sessions must open with
`resource_output_clock` containing a stream-relative epoch and host origin; the
request must include `resource_callback_contract:"cross_service_safe"`; the
reply echoes both it and `strict_source_echo`. Submitted host-monotonic frame clocks
must match that epoch, nested and outer timestamp/sequence must match, and source
sequences must strictly increase. Output timestamps subtract the negotiated host
origin. Unknown flags, clocks and unsupported resources are rejected; accepted
frames retain no borrowed storage. The fixture emits fake, non-decodable packets.
