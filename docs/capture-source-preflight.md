# Read-only capture source preflight

Capture v1.11 optionally adds `source_preflight` after the complete existing
v1.10 native prefix. Its interface identity remains the exact
`amaterasu.video-capture.native/1` family selected through trusted composition.
The SDK binds that family with both the new minor and complete size. An older
provider is explicitly unsupported; there is no untyped or capture fallback.

`amaterasu_module_sdk::capture` supplies request/response types, bounded JSON
encoding, native binding, and a `SourcePreflightFactory` adapter. The adapter
delegates ordinary capture binding unchanged, preserving its exact advertised
header and optional feature set. Only an explicit v1.11 query receives the
extended table; forwarding preserves unsupported optional operations. Its unsafe constructor requires an inner factory whose
bind allocates only binding metadata. The provider implements the pure query;
the adapter never supplies a default implementation or declares a provider ready.

A query describes `managed_output`, `desktop_output`, or `synthetic`, an optional
opaque source selector, and requested width, height, refresh in millihertz and
HDR source state. A missing selector permits checking a future managed-output
class before the display exists. Bounds are not a lease or a promise that every
intermediate mode works: always query the exact proposed mode.

`mode.bit_depth` is optional before stream negotiation. Absence is unknown,
not eight-bit. A final request supplies the negotiated depth, including ten-bit
SDR independently from HDR. Capabilities list accepted `bit_depths`; an empty
list is unknown and cannot authorize ten-bit input. `format_open: true` explicitly
declares support for an Open `source_format` field. Its SDK `SourceFormat`
contains bit depth and dynamic range, independently of encoder codec/output.
Consumers preserve the
legacy Open body when that marker is false, including generated-black sources
whose resource negotiation selects P010 without a new Open field.

The response echoes the request and returns `ready`, `unavailable`, or
`unsupported`, a bounded reason code, and mode capability bounds. The SDK rejects
wrong echoed facts, malformed bounds, excess replies, and readiness unsupported
by the returned capabilities. Requests and responses are at most 8192 bytes.

A provider must authenticate the bound caller and inspect actual compiled
support, configured permission and current backend/session readiness where it
can do so without effects. It must not start capture, create or mutate displays,
spawn workers, grant permissions, reserve native resources, or infer readiness
from the presence of a service alias. Readiness remains an observation; later
capture admission must repeat its own authorization and exact-source validation.

The public Video operation is `preflight_source` on `amaterasu.video/1`:

```json
{
  "action": "preflight_source",
  "route_key": "managed_output",
  "source_class": "managed_output",
  "route": null,
  "mode": {"width": 1920, "height": 1080, "refresh_millihz": 60000, "hdr": false}
}
```

`source_preflight_routes` in signed Video settings resolves the opaque route key
to an approved selected capture provider. Requests cannot choose a service.
Callers use `source_preflight_payload` and `decode_video_source_preflight` and
wait for a validated ready result before creating a monitor or changing display
state. Successful replies contain `{"ok":true,"preflight":...}`.
