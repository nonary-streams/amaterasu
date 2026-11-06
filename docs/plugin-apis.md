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
