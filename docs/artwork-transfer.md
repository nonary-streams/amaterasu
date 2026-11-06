# Artwork transfer

Artwork uses the existing `amaterasu.binary-transfer.native/1` SDK, with an
artwork schema layered on it. No image bytes travel through broker JSON.
The shell and the generic binary ABI have no artwork-specific code.

## Provider contract

A trusted startup-selected provider implements `artwork_open`. The built-in
library receives `{id, revision?}`; provider adapters receive their opaque
`reference`, while built-in Steam/Lutris/Playnite adapters translate catalog
references locally. A successful control response is `{ok: true, offer: ...}`.
An offer contains revision, MIME, total size, channel ID, and the exact native
channel description. It never contains a path, service selector, or image data.

Use `artwork_transfer::Source` to publish complete validated PNG, JPEG or WebP
bytes for the broker-attested **immediate caller**. Use `Source::register` under
the control service alias and grant `native_provide` for that alias. Consumers
need both `call` and `native_use`, plus the provider startup dependency. Native
modules are trusted code; these grants are routing/authority checks, not a sandbox.
`ProviderAdapter::register_artwork` supplies the same adapter to external plugins;
call `reap_artwork` on idle provider ticks as well as during dispatch.

The public `ARTWORK_FORMAT` and `ARTWORK_METADATA` schema constants identify
the record independently of any service alias. Non-Rust implementations can
use the existing `sdk/include/amaterasu_data_plane.h` interface with those schemas.

The standard source chunks at 256 KiB. `with_chunk_bytes` supports smaller
budgets, and consumers validate the advertised limits rather than requiring a
fixed chunk size. Chunks form one contiguous record with an immutable revision,
MIME and declared total. The artwork policy caps an image at 16 MiB and each
source at four channels/64 MiB of queued payload reservations. Those are artwork
policy limits, not limits of the reusable binary SDK.

`artwork_transfer::receive` binds the typed interface, verifies the negotiated
descriptor, checks every record/offset/length, and consumes the bounded record.
Dropping the binding retires queued bytes synchronously. Unclaimed offers expire
after 30 seconds; providers must tick `Source::reap`, and revocation should call
`Source::clear`. Malformed/expired offers fail closed. These sources publish the
whole bounded record before completing control negotiation, so a slow network
client cannot stall a native callback. HTTP retains a bounded assembled response.

The library resolves the exported catalog reference, opens the selected provider,
consumes its native record, and publishes an owner-bound record to its caller.
External providers and built-ins use the same transfer adapter. A requested
revision must match the resolved reference; each native record pins the bytes.

The catalog's `provider_artwork_refresh` operation accepts `{source_id, artwork}`
only from a configured importer. It updates an existing reference for the same
provider and image identity with a collection-revision check, preserving the
application's other fields and ownership marker. It cannot create an entry or
substitute another application's image. Steam uses this operation independently
of catalog auto-sync, so changing a cached cover does not require re-importing
games or overwriting application settings. Missing library paths still prevent
destructive reconciliation; individually validated current covers remain usable.

## Browser and Moonlight

Authenticated `POST /v1/library/artwork` accepts `{id, revision?}` and responds
with image bytes, a validated `Content-Type`, exact `Content-Length`,
`Cache-Control: no-store`, and `X-Content-Type-Options: nosniff`. Steam and Lutris
preview routes are `/v1/steam/artwork` and `/v1/lutris/artwork`, retaining their
existing read scopes. Cookie/CSRF, bearer, role and origin checks run before
opening a native channel. The browser bounds reads, checks the final length,
aborts removed views, and revokes image Blob URLs. No per-chunk HTTP calls occur.

Moonlight's boxart endpoint is NVHTTP `/appasset` (rather than RTSP). It opens
one library artwork record and responds with a binary image under the existing
paired-TLS/list permission checks and four-response admission budget. RTSP
stream setup and video packetization are unchanged.

The old JSON `artwork_read`/`artwork_release` interface is removed from these
consumer paths. Playnite's external extension still speaks its own authenticated
named-pipe chunk protocol; the Playnite provider assembles and validates that
record internally, then publishes it through the common native SDK. It retains
bounded asynchronous release retries until the extension acknowledges cleanup.
Changing that external extension's protocol requires its separately maintained
implementation, not a larger broker envelope.

## Validation boundaries

Tests cover alternate native providers and chunk budgets, owner mismatch,
non-rebindable channels, maximum-sized records, expiry, capacity recovery,
HTTP auth/role/scope and binary framing, browser cancellation and Blob lifecycle.
Local source/tests do not establish installed-host, Windows named-pipe, real
browser or real Moonlight delivery. Those require separate runtime qualification.
