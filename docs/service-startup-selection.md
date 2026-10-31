# Startup service selection

Service selection is opt-in. Legacy packages with no declarations and policies
with no selections retain their existing registration behavior and serialization.
Nonempty selection fields require a runtime that understands them: older runtimes
reject the new configuration field. No ABI version or installed grant is changed.

The signed component manifest may declare `service_provides` and
`service_requires`. Each declaration has `transport` (`raw`, `api`, or `native`),
`service`, `contract`, `major`, `minor`, and an optional `capabilities` list.
API declarations also name `method`; other transports must omit it. Requirements
may set `optional`. The default is required. The trusted bootstrap policy's
`service_selections` has `broker` and `native` maps, each keyed by service name.
A value such as `{ "provider": "example.provider", "contract":
"example.echo.contract/1" }` selects that principal and contract; `null`
explicitly disables the service. A component declaration never chooses its own
provider or creates a grant.

At verification, selected providers must exist, advertise the selected contract,
and possess the effective provide grant. A selected consumer must possess the
corresponding call or native-use grant, depend directly on the provider's exact
component version (unless it is the provider), and require only capabilities the
provider advertises. Major versions match exactly. Native minor versions are a
minimum; raw and API minors match exactly. API requirements also require an
existing installed user API method approval for that exact version and consumer.
The installed approval and runtime method/schema checks remain authoritative.

A missing selection for an optional requirement yields an absence diagnostic and
a disabled runtime entry. Required absence and any explicit invalid selection
fail verification, including for optional consumers. An installed API approval
may remain present for a disabled service, but registration is denied. The host
records optional absence in its product log when logging is available; verified
packages also expose structured absence diagnostics.

The broker and native registry check the selected provider before registration;
the native registry also checks the typed contract before reading its pointer.
Legacy untyped native registration works for selected services only through its
existing explicitly approved legacy contract mapping. There is no live provider
replacement. A raw broker route has no runtime typed contract proof. Signed
metadata describes provider claims; it does not prove implementation, native
capabilities or delivery. Native bind negotiation still checks the contract,
major/minor and table size, and a provider can fail to register or bind despite
passing metadata validation. Native code is trusted, and grants are not a sandbox.
