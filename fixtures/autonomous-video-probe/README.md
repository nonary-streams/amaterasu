# Autonomous synthetic video probe

This development-only fixture self-starts without a control request. It opens the existing synthetic NV12 capture fixture through the real video service, selects software x264, collects a bounded H.264 stream, and closes the video stream. The signed harness verifies the decoded alternating panel pattern with FFmpeg and checks that removing the probe's native-video grant causes a specific permission denial and confirmed cleanup.

The fixture accepts **only** the synthetic source and dimensions. It does not create or capture a display, access DRM, use NVENC, request privilege, or qualify hardware. Its lifecycle unit tests simulate cancellation, late responses, failed admissions, and cleanup retries; they do not inject actual broker races. An interrupted or destroyed host during a request is not proven to clean up, so use an external cgroup or equivalent supervisor to contain and drain the complete host process tree. An unknown open or cleanup that cannot be reconciled is reported as indeterminate.

## Dependencies

This fixture shares the workspace's SDK/media dependencies and requires the existing capture-source fixture, release profile, host test helpers, FFmpeg/ffprobe, x264 build prerequisites, and Linux dynamic library naming.

The harness rejects optimized Python because its validation uses assertions.

The existing software-probe profile names the development-only principal `amaterasu.media-probe` and library `amaterasu_media_probe`. The harness copies artifacts to its own temporary directory, substitutes this fixture's uniquely named library only in that copy, and signs/stages a fresh isolated test package. It never changes the original build artifact or production package. Its disabled listeners are configured on loopback with ephemeral ports.

## Build and run

From the repository root, use a separate build directory with sufficient free space. Set `CARGO_TARGET_DIR` to that directory in your shell. Then run:

```sh
cargo build --workspace --offline --locked --features amaterasu-encode/software-x264
cargo test -p amaterasu-autonomous-video-probe --offline --locked
cargo fmt -p amaterasu-autonomous-video-probe -- --check
cargo clippy -p amaterasu-autonomous-video-probe --offline --locked --all-targets -- -D warnings
python fixtures/autonomous-video-probe/test_verify.py
python fixtures/autonomous-video-probe/test_supervisor.py
python fixtures/autonomous-video-probe/signed_harness.py
```

The harness copies development libraries and creates signed/staged package copies, so allow several gigabytes of temporary space. `--target` points to a different directory containing already-built host and release executables and libraries; `--fixture-library` points to the newly built fixture library if it is in a different target directory. Both accept normal relative or absolute paths. These overrides must refer to compatible builds from the same source revision.

The host's control input is held open without sending a request; the fixture starts itself. The harness starts it in a new Linux session, waits a bounded time, independently decodes the output, closes the input, and on every normal or exception path signals remaining processes in that session with bounded TERM/KILL escalation and checks that the session has no live processes. It requires readable procfs and pidfd signalling, and fails if that verification is unavailable or output pipes remain open. It removes the temporary bitstream. This is experimental cleanup, not containment: a descendant can escape with `setsid`, and abrupt harness termination cannot execute cleanup. Use an external cgroup or equivalent supervisor for process-tree containment. The probe caps collection at 2 MB and uses separate 20-second work and 10-second cleanup deadlines. It writes its output with exclusive creation and mode 0600 under the process temporary directory and removes a partial write on failure.
