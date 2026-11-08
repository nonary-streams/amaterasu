# Audio SDK extensibility checklist

This records the first-revision implementation boundary. The SDK provides extension points; codec implementations, compatible clients, hardware qualification, and live playback are separate work.

## Contracts

- [x] Namespaced codec, sample, packing, framing, clock, layout, metadata and option-schema identities, without a central vendor codec enum.
- [x] Explicit PCM sample storage, valid bits, packing, sample rate and ordered speaker positions, including height and provider-defined layouts.
- [x] Format-neutral units with bounded payload and opaque metadata; codec initialization and stream metadata are separate from unit data.
- [x] Stream clock, timestamps, duration, encoder delay, configuration generation, discontinuity and end markers. Provider-defined flags and final payloads are preserved.

## Providers and orchestration

- [x] Typed SDK control messages for enumeration, capabilities, probing, open, effective configuration, status, reconfiguration and close, with built-in capability reporting and explicit unsupported responses.
- [x] Concrete codec/layout/framing/clock/metadata offers, latency and buffer bounds, client offers, and selected-provider probes before a custom chain opens.
- [x] PCM sources, already encoded or mutually understood spatial sources, and encoded passthrough.
- [x] Optional selected PCM processor, with independently negotiated input and output and declared latency. A processor may own a chain internally.
- [x] Bounded borrowed-buffer copying, backpressure, drain and owned retirement across the host plane and providers.
- [x] Trusted startup provider selection, typed interface identity, explicit grants/dependencies, and pinned provider identities for cleanup.
- [x] Generic transport submission; the built-in GameStream provider keeps its Opus framing, FEC and packet restrictions within that provider.
- [x] First-revision Rust and C media interfaces, in-tree capture/Opus/transport/stream-session migrations, probes and fixtures. RTSP retains its GameStream behavior.
- [x] Host-coordinated reconfiguration of a prepared custom session, including provider probes and advancing its generation. A failed partial update retires the session. Active-stream reconfiguration is not supported.
- [x] Audio-output and client UDP/TCP helpers, with bounded queues, caller-controlled pacing and cancellation. Local admission does not establish client delivery.

## Developer guidance and verification

- [x] [Audio plugin guide](audio-plugin-sdk.md), including composition, options, metadata, processing, passthrough and packaging.
- [x] [Rust](../crates/module-sdk/examples/audio_encoder_provider.rs) and [C](../sdk/examples/audio_encoder_provider.c) encoder table skeletons.
- [x] Focused contract, negotiation, replacement-provider, variable-format, metadata, bounds, backpressure, drain and cleanup tests; C layout checks, formatting and Clippy.

The built-in capture and GameStream/Opus providers advertise their actual capabilities; a 7.1.4 capture source, custom codec and compatible client must be supplied by corresponding plugins. No hardware or client playback qualification is implied.
