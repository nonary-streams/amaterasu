# Session lifecycle foundation

This module currently provides durable, caller-scoped admission receipts for
session preparation. A prepare receipt remains pending; it does not certify
provider preparation and must not authorize application launch. After restart,
in-flight receipts require recovery rather than replay. Installed policy must
explicitly grant each caller each supported operation.

The resource planning, permit and cleanup ledgers are independently tested
primitives. They are not yet connected to the coordinator. Provider dispatch,
stream lifecycle, participant cleanup, expiry, input bindings and action permits
are not implemented. Do not enable this service as a replacement for the existing
session path until those features are integrated and validated.
