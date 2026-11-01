# Negotiated video route fixtures

Two separately built native provider artifacts exercise the managed RTSP-to-session
integration test. Their output bytes are synthetic and do not implement a codec.
Build the fixtures and run the ignored test with the two library paths. For example,
on Linux from the repository root:

```sh
cargo build --offline --manifest-path fixtures/negotiated-video-providers/Cargo.toml
AMATERASU_ROUTE_PROVIDER_ONE="$PWD/fixtures/negotiated-video-providers/target/debug/libamaterasu_video_route_one.so" \
AMATERASU_ROUTE_PROVIDER_TWO="$PWD/fixtures/negotiated-video-providers/target/debug/libamaterasu_video_route_two.so" \
cargo test -p amaterasu-stream-session managed_rtsp_negotiates_the_real_session_route -- --ignored
```

When using `CARGO_TARGET_DIR`, substitute that target directory in both paths. The
loader keeps each library and its factories alive until every binding retires.
The test binds only ephemeral loopback ports.

The same two artifacts implement an explicit synthetic AFM1 framing operation.
This socket-free test runs each one through the unchanged managed session
coordinator and native consumer, including transport and retirement retries:

```sh
AMATERASU_ROUTE_PROVIDER_ONE="$PWD/fixtures/negotiated-video-providers/target/debug/libamaterasu_video_route_one.so" \
AMATERASU_ROUTE_PROVIDER_TWO="$PWD/fixtures/negotiated-video-providers/target/debug/libamaterasu_video_route_two.so" \
cargo test -p amaterasu-stream-session --offline managed_session_pipeline_uses_two_metadata_framing_artifacts -- --ignored
```

The Video provider and transport are synthetic in that test. It does not prove
an HDR bitstream, a real client, hardware support, or signed package loading.
