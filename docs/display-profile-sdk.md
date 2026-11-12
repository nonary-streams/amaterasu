# Selected HDR color-profile providers

The public `display_profile` SDK provides opaque catalog selections, read-only
resolution, durable prepare/apply receipts, and observed restore/recovery.
The provider resolves a catalog ID; callers cannot submit a filesystem path.
Version 2 binds each receipt to an exact lifecycle session. Replacements use
the same versioned requests and must retain ambiguous native outcomes.

The built-in display provider supports these operations on Windows and returns
a typed unsupported result on Linux. Trusted startup settings explicitly select
`hdr_profile_api_versions: ["v2"]` (or `v1` for unbound legacy consumers) and
provide the exact corresponding services, approved methods and State grants.
Empty selection keeps the default display provider registrations unchanged.
The public methods are declared in the display component's
`metadata/profile_api_v2.json` and `metadata/profile_api.json`; a signed package
selecting them must include their exact method approvals.

Profile I/O, Windows calls and durable transactions run on the display service
worker. Resolution binds the exact display and policy revisions; preparation
persists the receipt before mutation, and restore verifies observed state.

This recovers the general provider and SDK. Automatic selection from imported
Apollo client policy is excluded. No Apollo compatibility provider, imported
permissions or implicit profile selection is enabled. The unused NVHTTP stage
placeholder is removed; it had no resolver or reachable transition. An approved
consumer uses the public API and retains its receipt through session cleanup.

Linux replacement-provider tests exercise the transaction state machine. Native
Windows profile application still requires platform qualification.
