# Audio plugin SDK

Audio capture, encoding, and media transport are independently selected by trusted startup composition. The stream coordinator invokes those providers through their typed SDK interfaces; native plugins are trusted code and grants are not a native-code sandbox.

## Custom audio sessions

An authorized protocol provider may prepare a session with an `audio` object containing `channels`, `channel_mask`, `layout`, `packet_ms`, `bitrate`, and `custom`. The `custom` object contains a namespaced `codec`, `sample_rate`, a `stream` description, a `pcm` descriptor and provider-defined JSON `options`, bounded to 64 KiB. Every identifier contains nonzero `namespace`, `value`, and `version` fields; unknown identifiers are available to providers without changes to the host. The PCM descriptor supplies the sample format, storage bytes, valid bits, packing, and an ordered list of speaker identifiers. The list describes actual sample order, including height channels, without inferring order from a channel mask. Standard speaker values use the corresponding WAVE speaker-position bits; provider-defined positions have their own namespace.

Provider and processor options may carry a namespaced `options_schema` identifier so independently developed consumers can recognize the option format and its revision. Empty or otherwise self-describing options may omit it.

The stream description supplies namespaced framing and clock identities, a nonzero configuration generation, and encoder delay in microseconds. Optional schema-identified codec initialization bytes and stream metadata are passed separately from packets, with bounded total storage. The participants agree on the clock's epoch; packet timestamps and duration use microseconds on that clock. Every unit carries the generation of its decoder configuration, and the host checks it against the agreed stream. Discontinuity and end flags mark changes in continuity and stream retirement; other provider flags remain opaque.

For example, an authorized protocol can request this stereo profile (the codec identifier is illustrative):

```json
{"channels":2,"channel_mask":3,"layout":"stereo","packet_ms":10,"bitrate":128000,
 "custom":{"codec":{"namespace":42,"value":99,"version":1},
           "sample_rate":48000,
           "stream":{"framing":{"namespace":42,"value":3,"version":1},
                     "clock":{"namespace":1,"value":1,"version":1},
                     "layout":{"schema":{"namespace":1,"value":1,"version":1},
                               "positions":[{"namespace":1,"value":1,"version":1},{"namespace":1,"value":2,"version":1}]},
                     "generation":1,"encoder_delay_us":0},
           "pcm":{"sample_rate":48000,"sample_format":{"namespace":1,"value":1,"version":1},
                  "sample_bytes":4,"valid_bits":32,"packing":{"namespace":1,"value":1,"version":1},
                  "speakers":[{"namespace":1,"value":1,"version":1},
                              {"namespace":1,"value":2,"version":1}]},
           "options":{"profile":"example"}}}
```

The coordinator passes the complete `audio_profile` in encoder, media transport, and capture open requests. Each provider must echo it unchanged in its successful response; the transport also includes it in its status responses. The encoder also reports the requested `channels` and `sample_rate`; its optional `packet_bytes` is informational and may be omitted for variable-size output. The custom transport owns its framing, packet sizing, encryption and FEC; the coordinator passes `wire_payload_bytes: 0` for this profile. Capture receives and confirms `pcm_format`, then supplies frames with the negotiated representation. A mismatch is rejected with normal owned cleanup. Recovery from an encoder failure must report the same `audio_profile`.

Standard PCM supports 16-, 24- and 32-bit integer samples and 32- and 64-bit floating point samples, with either interleaved samples or contiguous channel planes. Custom formats and packing carry their own identifiers. Each frame is bounded to 1 MiB; standard packing has exactly the size implied by its samples. Providers define the size and meaning of custom packing and must agree on it before opening the stream.

The authenticated caller and selected protocol provider are responsible for negotiating a client that understands the chosen codec and transport. The built-in GameStream transport accepts its Opus profile and does not claim custom codec support. Selecting another provider requires the matching signed dependencies and grants; selecting an alias does not grant authority.

The built-in capture provider can participate when the requested PCM format is 48 kHz float with 2, 6, or 8 channels. A profile requiring another capture format or a 7.1.4 capture layout needs a capture provider that offers it. The SDK accepts those layouts; it does not create missing platform capture capabilities.

## Processing PCM

Trusted startup composition may select an `audio_processor` service. A PCM custom profile can request it by adding `processing` with an `output` PCM descriptor, declared `latency_us`, and provider-defined `options`. The source `pcm` descriptor describes capture; the processing output describes encoder input, allowing the rate, sample format, packing, and channel order to change. The session's `channels` and `sample_rate` describe the encoder output profile; the start request's channel count describes the capture input. The host opens the selected processor with the complete profile before the encoder, verifies its confirmation, and invokes its typed `amaterasu.audio-processor.native/1` interface. A processor can implement an internal chain of effects, mixing, or resampling; the coordinator does not insert an unselected default.

`submit` accepts an entire input frame or returns busy without accepting it. `receive` delivers at most one borrowed output, retaining it if the sink rejects it. `drain` stops input; `receive` returns closed only after all output has been delivered. The host holds pending input and output in bounded storage and drains the processor before draining the encoder. Processor configuration and PCM descriptors are frozen for the session; a live reconfiguration contract is not yet provided.

For a non-default service, the coordinator's open request includes `required_native_contract`; the response must echo it as `native_contract`. This control-plane acknowledgment is separate from typed native registration and the minor-version checks below.

## Offers and control messages

Rust providers and protocol plugins can use `AudioFormatOffer` for concrete codec, framing, clock and layout combinations, with metadata schemas, payload bounds and a latency range. `negotiate_audio_offers` finds a combination common to the supplied participants and intersects those limits; it reports a separate error if its bounded search limit is reached. A protocol may include `client_offers` in the session audio request: the host checks that the chosen custom profile appears in the advertised offers and enforces that offer's unit and metadata bounds. When offers are omitted, the protocol is responsible for its own client negotiation.

`AudioProfile`, `AudioSessionProfile`, `AudioControlRequest`, `AudioProbeResult`, `AudioCapabilitiesResult`, and `AudioOpenResult` provide serializable building blocks for enumerate, probe, open, effective configuration, status, reconfigure, and close operations. Before opening custom media, the coordinator sends the SDK `probe` request to the selected source, optional processor, optional encoder, and transport. Each responds with `AudioProbeResult`, echoing the profile and declaring support; a rejection aborts preparation with `unsupported_audio_format`. The selected provider must report an unsupported combination rather than substitute a different codec.

Before an authenticated stream starts, its owner may submit `reconfigure_audio` with the session and a complete replacement `audio` object. The generation must increase, and the source kind and presence of a processor must stay the same. The host probes all selected roles first, then sends `reconfigure` to the already opened processor, encoder, and transport as applicable. A provider confirms the same effective profile and stream handle; a transport must retain its previously reported local addresses. Probe rejection preserves the prepared session. If an update fails after a provider may have changed, the session is retired through its normal cleanup instead of exposing a partially updated configuration. Live reconfiguration of an active stream is not supported.

Start from the intentionally incomplete [Rust encoder table skeleton](../crates/module-sdk/examples/audio_encoder_provider.rs) or [C table skeleton](../sdk/examples/audio_encoder_provider.c). Implement the hooks, expose a factory under the matching typed contract, and add the selected service, dependency ordering, and grants to the signed composition. Give each stream an owner-scoped control handle, retain callbacks and code until the host releases its bindings, and retire workers before unloading. The skeletons do not contain a codec.

## Encoded units

Encoder providers register the typed `amaterasu.audio-encoder.native/1` contract and deliver `AudioUnit` through `AudioEncoderApi.receive`. Media providers register `amaterasu.media-transport.native/1` and accept the same units through `MediaTransportApi.submit_audio`. These are the first-revision interfaces for both built-in and custom codecs. PCM submission and drain methods remain responsible for encoder input and retirement.

In Rust, `audio::AudioOutput::from_sink` wraps the host-supplied sink inside `receive`. Use `send_packet`, `send_packet_with_flags`, and `finish`; translate errors with `AudioOutputError::status`. `AudioUnit` carries the negotiated format identity, stream-relative timestamp and duration, sequence, and optional schema-identified metadata. Metadata is opaque to the coordinator and is forwarded with its packet. Payload and metadata are borrowed for a synchronous callback; a consumer that retains them must copy them. Busy accepts nothing and allows retry. The end flag can accompany a final payload or metadata, or `finish` can send an empty terminal unit. The host bounds pending storage and retains both payload and metadata while a transport is busy.

```rust,ignore
// `sink` is borrowed from the current receive callback.
let mut output = unsafe { AudioOutput::from_sink(sink, negotiated_codec, generation)? };
output.send_packet(encoded_bytes, timestamp_us, duration_us, sequence, metadata)?;
```

The matching C declarations are in `sdk/include/amaterasu_media.h`. Interface size/version checks complement, but do not replace, the typed native contract identity and the control-plane profile agreement.

## Already encoded sources

A custom profile may set `source` to `encoded` and omit `pcm` when its selected capture provider already produces the negotiated codec or another mutually understood spatial representation. The capture open request specifies `audio_source: "encoded"`; the provider confirms it, echoes `audio_profile`, and registers `amaterasu.encoded-audio-source.native/1`. Its `receive` delivers the same `AudioUnit` type as an encoder, and `drain` stops new input and eventually delivers an end unit. The coordinator binds this source directly to the selected transport and does not open an encoder. It retains the source until its queued output and transport drain have completed. Codec setup and metadata remain part of the profile and units agreed by the source, transport, and client.

## Network delivery

Transport implementations use the Network SDK. `audio::ClientAudioChannel` wraps a Network SDK UDP socket and a session-selected destination, or an already connected session socket. `send_datagram` preserves a complete datagram and reports provider backpressure; it does not prove remote delivery. `QueuedAudioDatagrams` adds bounded packet storage and a caller-controlled send budget. `ClientAudioStream` similarly wraps a connected Network SDK TCP stream, retaining unsent bytes across partial writes. Their `close` methods cancel pending data and release their socket guards. The selected transport owns peer selection, framing, pacing decisions and wire encryption; the host does not provision these sockets on behalf of an encoder.

The unit and channel APIs provide extension points; they do not implement a codec, negotiate a particular client's decoder, or provide Atmos rendering. Real codec and client interoperability must be established by the corresponding plugins.
