# vibeshine_drm DMA-BUF capture

This independently built trusted native library provides control and native
`amaterasu.capture-vkms/1` for the owned virtual outputs that the
`vibeshine_drm` kernel driver exposes as `Virtual-N` connectors. Frames are the
driver's latest completed primary-plane scanout of the output's CRTC, delivered
as borrowed DMA-BUF descriptors without any CPU copy. Native API grants do not
sandbox native code. The provider exists only on Linux; other targets build an
empty library.

The driver exports frames only to callers with `CAP_SYS_ADMIN`. The host
process never receives that capability. A small dedicated helper,
`amaterasu-vkms-frame-helper` (crate `crates/vkms-frame-helper`), holds it and
does nothing else.

## Installing the helper

Install the helper as a root-owned file that group and others cannot write,
for example `/usr/libexec/amaterasu/amaterasu-vkms-frame-helper` with mode
0755, then grant it exactly one permitted file capability:

```sh
sudo setcap cap_sys_admin+p /usr/libexec/amaterasu/amaterasu-vkms-frame-helper
```

Grant `+p` only, never `+ep` or other capabilities. The helper refuses to run if
it holds any other capability after dropping, or if any real, effective or saved
user or group id is root. The host service must not set `NoNewPrivileges=yes`,
must keep `CAP_SYS_ADMIN` in its bounding set, and the file system holding the
helper must not be mounted `nosuid`; otherwise the kernel does not apply the
file capability and the provider reports `permission_denied`.

## Settings

```json
{"enabled":false,"capture_principals":[],"frame_helper":null}
```

An administrator-controlled startup policy enables capture, allowlists
authenticated immediate callers (at most 16) in `capture_principals`, and sets
`frame_helper` to the helper's absolute path. Each time the helper is used the
module opens that path and checks the opened file: a regular executable file
owned by root, not writable by group or others, and without setuid or setgid
bits. It then executes that same opened inode, so replacing the path afterward
cannot substitute another program.

## Control actions

The actions and reply shapes follow `amaterasu.capture-pipewire/1`. Failures
reply `{"ok":false,"error":{"code":"...","message":"..."}}`.

- `{"action":"status"}` reports `enabled`, `native_supported`,
  `frame_helper_configured`, the `availability` from the latest enumeration,
  the caller's `streams` (with `frames_delivered` and any `fault`), and lease
  counters `opened`, `closed`, `active`.
- `{"action":"enumerate"}` requires a capture principal. Optional `offset`
  must be zero and `limit` is 1 through 256. It returns every `Virtual-N`
  connector on every vibeshine_drm card as `outputs` entries with `id`
  (`kwin:Virtual-N`), `device_name` (`Virtual-N`), `attached` (connected with an
  active CRTC and mode), `card`, `connector_id`, `connection`, `crtc_id`,
  `width`, `height`, `refresh_hz`, `format:"dmabuf"` and `desktop_geometry`.
  `availability` has `available`, `reason` and an operator `message`; `checks`
  reports the helper and driver separately. Reasons are
  `frame_helper_unavailable` (not configured, unusable file, or the helper
  refused to start), `permission_denied` (the helper lacks `CAP_SYS_ADMIN`; the
  message names the `setcap` command) and `driver_unavailable` (no readable
  vibeshine_drm card). Enumeration starts the helper once to check it.
- `{"action":"open","output":"kwin:Virtual-9","operation_id":"video-1"}`
  requires a capture principal and an operation id. `constant_rate:true` fails
  with `unsupported_constant_rate`. The output is resolved to its current CRTC,
  a helper is started for the stream, opens the card and exports one frame to
  prove the path; only then is the stream reported. The reply carries
  `stream`, `output`, `operation_id`, `card`, `connector_id`, `crtc_id`, the
  mode, `frame_transport:"dmabuf"`, `geometry_supported:true` and
  `desktop_geometry`. Replaying the same operation id returns the same stream;
  a different output with that id is `operation_conflict`. Other definite
  refusals include `invalid_named_output`, `output_not_found`,
  `output_disconnected`, `output_inactive`, `output_ambiguous`,
  `output_unavailable`, `frame_helper_failed` and `resource_exhausted` (at most
  4 open streams and 128 per module instance).
- `{"action":"lookup","operation_id":"video-1"}` recovers the caller's stream.
- `{"action":"close","stream":123}` is idempotent for its owner. It stops
  admitting native reads at once and ends that stream's helper; a read admitted
  before close finishes with its descriptors still valid.

## Output resolution

`kwin:Virtual-N` names the DRM connector `Virtual-N` (connector type Virtual,
type index N). N is whatever the driver assigned and changes when the module is
reloaded, so any N >= 1 is accepted. Resolution reads only entry names and
`cardN/dev` under `/sys/class/drm` to find cards with a `cardN-Virtual-N`
connector, then opens `/dev/dri/cardN` read-only, confirms the device number
and that `DRM_IOCTL_VERSION` names `vibeshine_drm`, and issues read-only
GETRESOURCES, GETCONNECTOR, GETENCODER and GETCRTC queries to find the
connector's encoder, CRTC and mode. The descriptor is closed afterward. If the
kernel made it DRM master because no compositor held the device, master is
dropped immediately so the compositor can still acquire it; no other DRM state
is touched. Open streams re-resolve once per second, and immediately when the
helper reports the CRTC is gone: a CRTC or mode change is followed, while a
disconnected, inactive or replaced connector faults the stream.

## Geometry

No unprivileged source of the compositor's desktop layout is used. Geometry
reports the output as the entire virtual desktop at origin 0,0, sized by the
CRTC mode in pixels. Absolute input mapping therefore assumes this output is
the whole desktop at scale 1. The capture geometry callback returns the same
rectangle as the open reply; a mode change updates both, which consumers see
as a geometry change.

## Native frames

The native table is `CaptureApiV1_4` (major 1, minor 4). `read_frame`,
`read_frame_with_freshness` and `read_frame_with_capture_timestamp` return
`MEDIA_UNSUPPORTED`. `read_dmabuf_frame` synchronously asks the stream's
helper for the newest presentation, fills one `DmaBufFrame` and calls the sink
exactly once:

- fourcc, modifier, plane layout and per-plane acquire fences are the driver's.
  `color` is `DMABUF_COLOR_SRGB`.
- `sequence` is the driver presentation sequence. Freshness is
  `CAPTURE_FRAME_FRESH` when the sequence is newer than the last presentation
  the sink accepted on this stream, otherwise `CAPTURE_FRAME_CACHED_REUSE`; a
  rejected fresh frame stays fresh on the next read.
- `timestamp_us` is the driver's CLOCK_MONOTONIC presentation time in
  microseconds since the stream opened. A presentation completed before open
  maps to zero, and timestamps never decrease.
- The DMA-BUF and fence descriptors are borrowed for the callback only. The
  module closes them after the sink returns; a consumer that keeps a buffer
  must duplicate its descriptor first. An open DMA-BUF pins the allocation but
  does not stop the compositor from rendering into it again.

No frame yet is `MEDIA_PENDING`; a busy helper or controller is `MEDIA_BUSY`;
a closed or faulted-output stream is `MEDIA_CLOSED`; a failed helper or
permission refusal is `MEDIA_BACKEND_ERROR`. Each read is one helper round trip
bounded by 100 ms; a helper that misses it is treated as failed. The module
does not use the helper's WAIT operation, so reads never block on the next
presentation.

## Helper protocol

The module creates an AF_UNIX `SOCK_SEQPACKET` socketpair and starts the helper
with an empty environment, no arguments, null standard streams, `/` as working
directory and its socket end at descriptor 3; every other descriptor is
close-on-exec (Linux 5.11 or later). The helper exits when the socket closes,
and exit statuses 64-69 identify startup refusals.

Every record is one datagram of fixed length, little-endian, version 1. All
fields an operation does not use and every reserved byte must be zero;
anything else, a wrong length, or a request carrying descriptors receives a
`malformed_request` failure without touching the device.

Requests are 48 bytes: magic `AVFQ`, version u16, opcode u16, request id u32,
crtc id u32, device major u32, device minor u32, last sequence u64, timeout
u32, 12 reserved bytes. Opcodes:

1. HELLO: answers whether `CAP_SYS_ADMIN` is permitted.
2. OPEN_CARD(major, minor): opens `/dev/dri/card<minor>` read-write, requires
   major 226, the same `st_rdev` and the driver name `vibeshine_drm`, and drops
   any master role. One card per helper.
3. FRAME(crtc): driver GET_FRAME.
4. WAIT(crtc, last sequence, timeout <= 1000 ms): driver WAIT_PRESENT.

Responses are 128 bytes: magic `AVFR`, version, echoed opcode and request id,
status u32 (0 or a failure), errno detail, result flags, crtc id, plane count,
sequence, timestamp, width, height, fourcc, fence mask, modifier, four pitches,
four offsets and 24 reserved bytes. A ready frame carries one SCM_RIGHTS
message: each plane's DMA-BUF in plane order, then the sync_file of each plane
named in the fence mask. The helper closes its own copies after sending. The
client checks the request id, opcode, CRTC echo and exact descriptor count, and
stops using a helper after any transport or protocol fault.

Failures are distinct: `permission_denied` (no permitted `CAP_SYS_ADMIN` or the
driver refused it), `card_not_open`, `card_already_open`, `device_rejected`,
`device_unavailable`, `wrong_driver`, `crtc_not_found`, `busy`, `interrupted`,
`driver_error` and `invalid_driver_response`. The helper validates the driver's
answer as strictly as a request and closes every descriptor of a malformed one.

The helper sets `PR_SET_NO_NEW_PRIVS`, clears dumpability and the ambient set,
reduces its capabilities to at most a permitted `CAP_SYS_ADMIN` with nothing
effective or inheritable, and raises it to effective only for the duration of
one private ioctl, lowering it immediately (it aborts if lowering fails). It
never takes DRM master, commits state or writes properties.

## Qualification

Unit tests cover the wire protocol, descriptor passing over a socketpair,
the helper's request rules and capability handling with fakes, output names,
sysfs scanning of a fake tree, resolution and error mapping, freshness and
timestamps, descriptor lifetimes across the native callback, and helper-file
checks. An integration test runs the real helper binary without capabilities
and never asks it to open a DRM device. Capture from a real vibeshine_drm
device with `CAP_SYS_ADMIN`, the exported formats and modifiers KWin produces,
fence behavior, and the video and encoder import path remain separate
acceptance gates. Behavior was modeled on Vibeshine's KMS capture; no Vibeshine
source was copied.
