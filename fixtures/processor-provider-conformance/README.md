# Generic processor artifact proof

These two independently built synthetic processor libraries annotate a frame
with different color transfer metadata; they do not process pixels. A single
unchanged `NativePlane` test loads each library and drives the real generic
negotiation path through a mock typed runtime, then checks output and retirement.
This does not test signed package loading or real GPU/fence behavior.

```sh
CARGO_TARGET_DIR=/tmp/amaterasu-processor-fixtures cargo build --offline --manifest-path fixtures/processor-provider-conformance/Cargo.toml --workspace
PROCESSOR_FIXTURE_PQ=/tmp/amaterasu-processor-fixtures/debug/libprocessor_fixture_pq.so \
PROCESSOR_FIXTURE_SRGB=/tmp/amaterasu-processor-fixtures/debug/libprocessor_fixture_srgb.so \
cargo test -p amaterasu-video --offline --locked unchanged_video_plane_loads_two_processor_provider_artifacts -- --ignored
```
