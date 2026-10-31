# Video profile negotiation extension

The optional `amaterasu.video-profile.native/1` provider returns a bounded,
versioned snapshot of extension codec and framing identities and their GameStream
selector and RTP capability marker. The SDK registers and binds this exact
native contract; runtime grants and trusted startup composition select the
provider. The provider factory must remain live until all registrations and
bindings retire. Snapshot bytes are copied during a synchronous callback. Native
providers are trusted code; a capability declaration does not prove that a
codec or transport implementation exists.

`Negotiation::with_video_profiles` takes both the provider offer and a separate
trusted snapshot of exact downstream support. It rejects the entire offer if
any profile is missing from downstream support. These values must come from
trusted composition and actual resource/implementation checks, never the peer.
The constructor does not itself probe hardware or prove that support is live.
No production RTSP controller constructs this extension-aware path yet. The
current controller explicitly rejects any extension action because the generic
encoded-packet and framing path does not exist. Existing constructors and the
standard codec enum retain their behavior.

DESCRIBE emits a marker only for an admitted offer. ANNOUNCE chooses the exact
wire selector and carries a distinct `NegotiatedVideoFormat::Extension` through
`Action::ExtensionAnnounce`; it is never converted to H.264 or another legacy
format. Common bounds and encryption/control checks still apply. Only 8-bit SDR
4:2:0 is currently accepted for extensions; additional profile fields, framing
features, dynamic ranges and codec-specific SDP attributes need explicit later
contracts. Unknown critical attributes fail closed. Provider replacement tests
use the real runtime native registration/binding and actual RTSP state machine,
with synthetic profiles and a simulated peer. They do not establish actual
PyroWave, Dolby, decoder, hardware or network-delivery compatibility.

Standard formats still use the legacy built-in negotiation path. Moving that
provider behind the same selected interface is outstanding and should accompany
the production routing work; no live replacement is provided by this slice.
