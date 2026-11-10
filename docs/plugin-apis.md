# Operation APIs

`amaterasu_module_sdk::operation_api` adapts typed operation services to installed,
versioned API methods. The provider supplies explicit required/optional schemas,
registers those exact methods, and retains its existing caller authorization and
value validation. `decode_delivery` accepts legacy raw operations only on the
legacy service; a public API alias cannot become an unrestricted operation route.
API service names use a non-reserved namespace. Trusted package composition
selects the provider and callers; browser declarations alone do not grant access.

The Stats core plugin supplies read, settings, and history APIs from its own
repository. Its signed UI calls locally approved aliases through the shared SDK.
The stream provider exposes its own snapshot and selected-stream stop contracts.
An unchanged consumer is tested against two implementations through the real
broker, including schema identity, result consumption, and release.


`nonary.stream-status/1` snapshots optionally include `encode_timing` per stream.
Absent/null remains compatible with older video providers. A present value has
`version: 1`, `scope: "provider_input_to_output_ready"` and decimal-string u64
`epoch`, `completed_frames`, `total_us`, `min_us`, `max_us` fields. Strings preserve
exact cumulative values in browser clients. Derive mean milliseconds only from
newly measured frames between compatible epochs; no new measurement is unknown,
not zero or a repeated average. See [the encoder SDK](encoder-plugin-sdk.md#optional-measured-timing)
for timing bounds, optional native interfaces and provider responsibilities.


Core modules also supply these independently approved methods:

| Public service | Methods | Owning module |
| --- | --- | --- |
| `nonary.display/1` | `status`, `snapshot`, `enumerate`, `apply`, `restore`, `recover` | [Display](https://github.com/nonary-streams/amaterasu-display) |
| `nonary.catalog/1` | `get`, `update`, `update_provider_overrides` | Catalog |
| `nonary.host-settings/1` | `read`, `update` | NVHTTP settings provider |

The exact request/response schemas live in each module's `metadata/api.json`.
Catalog updates require an application ID and expected revision. A normal update
carries the complete entry; provider-managed entries use the override-only method.
Host settings updates carry `values` and `expected_revision`; omitted, null and
explicit values retain their existing distinct meanings. Readback confirms stored
values, not native application to a stream. Display effects retain provider-owned
mutation approval, topology checks, leases and durable recovery.

`operation_api::register_selected` registers only additive methods selected by
trusted composition. Older policies can continue using their original services;
unapproved public methods remain unavailable. API consumers never silently use
a raw legacy service or another implementation. The release profile imports the
module-owned schemas and grants only matching existing callers. The public
browser UI uses signed method aliases and its authenticated user's permissions.

The Plugins page inventory projects bounded identities from the exact
verified manifest selected by the running worker's digest. Missing or changed
manifest bytes leave inventory unavailable; the persisted selection is never
presented as proof of a running component's health. The status is read without
the activation lock, which the running service holds for its whole lifetime.
