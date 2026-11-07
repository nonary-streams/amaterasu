# Explicit CPU resource processing

Windows high-depth capture publishes standard CPU-v2 P010 frames with source
color and provenance. The stock GPU processor accepts DMA-BUF leases and cannot
consume those frames. No stock CPU tone mapper is selected. An HDR source with
an SDR client therefore needs an explicitly selected compatible resource
processor; a source-format declaration alone does not perform conversion.

The existing `FrameResourceProcessorApi` v1.0 is an independent native contract,
`amaterasu.frame-resource-processor.native/1`. Input offers describe accepted
source formats and colors. Output offers independently describe the pixels the
processor produces. Selecting an output never permits relabeling source pixels.
The provider must perform the actual transform, including any upload/readback,
and finish that work before lending CPU output. The host supplies no algorithm.

The public SDK's `processing::resource` module provides:

- `CpuResourceProcessor`, whose bounded `describe` returns separate input/output
  offers, `configure` atomically accepts their pairing, and `process` returns an
  independently owned `OwnedCpuFrame` for the selected output.
- `ResourceProcessorFactory` and `into_resource_processor_binding`, exporting
  that implementation through the same generic v1.0 table external native
  plugins use. `Factory::register` registers the exact typed family; retain the
  boxed factory until all consumers retire.
- `CpuResourceProcessingSession`, which negotiates an authentic capture stream
  and selected processor, checks every source/output tuple, generation, frame
  identity and clock envelope, and cancels synchronously on failure or drop.

Use `CpuProcessingOffer` with standard CPU-v2 capabilities and a complete known
color tuple. Return `CpuProcessingDescription` with a nonzero generation and at
most 64 unique input/output IDs. Descriptions are metadata only; the provider
owns immutable startup settings and per-stream resources. `CpuResourceInput`
borrows source pixels for one synchronous call. `cancel` and `shutdown` must
retire all work before returning. The SDK output borrow cannot be retained;
there is no hidden queue or lifetime extension.

For example, an independent implementation may advertise P010 with
BT.2020/PQ/BT.2020-NCL limited source color and NV12 with BT.709/BT.709/BT.709
limited output color. Another may produce BGRA8 BT.709/sRGB/full output. The
SDK replacement tests run both through the unchanged consumer using neutral
black transformations and verify the actual output codes. Those fixtures
qualify negotiation and ownership, not a general tone-mapping algorithm or GPU
or client presentation.

Video startup composition selects `resource_processor_service` instead of the
older CPU/GPU processor families, with an explicit dependency and native-use
grant for the exact family. A stream's existing lifecycle processing selection
must name that same service. v1.0 uses signed startup configuration; nonempty
per-stream settings and live updates are unsupported. Missing CPU source/output
support, unknown or contradictory color, missing capture provenance, and
incompatible output geometry all fail explicitly. There is no provider fallback
and no default package selection for this extension.

An external component can export `example.frame-effects/1` with
`ResourceProcessorFactory::new(create_for_authenticated_caller)`, where the
constructor returns its own `CpuResourceProcessor` configured from that
component's signed startup settings. Its managed module registers and retains
the boxed factory using `factory.register(&runtime, "example.frame-effects/1")`.
The factory authenticates the principal supplied by the native binding; the
implementation owns any additional per-caller policy.

Trusted composition then adds the processor component as a Video dependency,
grants Video native use of `example.frame-effects/1`, and sets Video settings:

```json
{"resource_processor_service":"example.frame-effects/1"}
```

The authenticated lifecycle stream selection names that exact service:

```json
{"service":"example.frame-effects/1","enabled":true,"source_hdr_required":true,"settings":{}}
```

Do not set either older processor-service field at the same time. The component
registers the exact processor contract rather than the consumer or GPU-lease
contract, and dependency order keeps it loaded until Video has cancelled every
processing session. Provider-specific startup settings remain in the provider
component; Video interprets neither its transform parameters nor its identity.
