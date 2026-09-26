# Global host configuration service

`amaterasu.host-configuration/1` is the authoritative v1 provider for shared
host settings. It stores the document in `amaterasu.state/1` under caller
`amaterasu.host-configuration`, key `global/amaterasu.host-configuration`.
State must be available to this module. The service registers even when its
trusted path root is missing or invalid, so callers can observe that it is not
ready instead of mistaking defaults for loaded configuration.

The host injects a per-user canonical absolute state root into this module's
component settings after package verification:

```json
{"host_state_root":"C:\\ProgramData\\Amaterasu\\runtime\\user-state\\S-1-5-21-1"}
```

`host_state_root` is host-owned. Package policy must not choose or override it.
The module validates that it names an existing canonical directory without a
symlink or reparse point. Until a valid root is available, reads and updates
return `configuration_unavailable`, and readiness returns `ready:false`.

The stored value is one document with exactly these fields:

```json
{
  "version":1,
  "revision":0,
  "file_state":"sunshine_state.json",
  "vibeshine_file_state":"vibeshine_state.json",
  "credentials_file":"",
  "pkey":"credentials/cakey.pem",
  "cert":"credentials/cacert.pem",
  "log_path":"amaterasu.log",
  "min_log_level":"product_default"
}
```

`file_state` and `vibeshine_file_state` name legacy import inputs; neither
selects Amaterasu's active runtime state store. An omitted or empty
`credentials_file` inherits `file_state`. `min_log_level` is a typed enum with
`product_default`, `verbose`, `debug`, `info`, `warning`, `error`, `fatal`, and
`none`. `values.min_log_level` preserves the selected policy. The effective
value resolves `product_default` using the built product channel: stable uses
`info` and prerelease uses `debug`.

Every request rejects unknown fields. The provider accepts `readiness` and
`get` from granted consumer modules, and accepts `update` only from the
authenticated immediate caller `amaterasu.management`. The management module
must verify the signed administrator request before making this internal call;
the provider does not treat native modules or API grants as a sandbox.

```json
{"action":"readiness"}
{"action":"get"}
{"action":"update","expected_revision":0,
 "file_state":"sunshine_state.json",
 "vibeshine_file_state":"vibeshine_state.json",
 "credentials_file":"",
 "pkey":"credentials/cakey.pem",
 "cert":"credentials/cacert.pem",
 "log_path":"amaterasu.log",
 "min_log_level":"product_default"}
```

A missing state key is the valid default v1 document at revision zero; it is
not written just by reading it. `readiness` performs an authoritative state
read when the trusted root is available. It returns ready only after that read,
document validation, trusted-root validation, and effective-path validation
succeed:

```json
{"ok":true,"ready":true,"version":1,"revision":0}
```

State transport failures, invalid stored documents, missing trusted root, and
unsafe stored paths return `{"ok":false,"ready":false,"error":...}` with
code `configuration_unavailable`. Submitted unsafe update values return
`invalid_settings`. This separates unavailable or not yet hydrated service
state from a successfully read default document.

`get` returns both stored settings and the resolved view:

```json
{"ok":true,"ready":true,"version":1,"revision":0,
 "values":{"file_state":"sunshine_state.json",
   "vibeshine_file_state":"vibeshine_state.json","credentials_file":"",
   "pkey":"credentials/cakey.pem","cert":"credentials/cacert.pem",
   "log_path":"amaterasu.log","min_log_level":"product_default"},
 "effective":{"file_state":"C:\\ProgramData\\...\\sunshine_state.json",
   "vibeshine_file_state":"C:\\ProgramData\\...\\vibeshine_state.json",
   "credentials_file":"C:\\ProgramData\\...\\sunshine_state.json",
   "pkey":"C:\\ProgramData\\...\\credentials\\cakey.pem",
   "cert":"C:\\ProgramData\\...\\credentials\\cacert.pem",
   "log_path":"C:\\ProgramData\\...\\amaterasu.log",
   "min_log_level":"info"}}
```

Relative paths resolve under the per-user host state root. Explicit absolute
paths remain absolute. `credentials_file` resolves to the effective
`file_state` when inherited. Path strings are limited to 4096 UTF-8 bytes;
control characters, NUL, parent traversal, and drive-relative Windows paths are
rejected. Existing relative path components are checked for symlinks/reparse
points when the document is read or updated. Consumers that later open these
paths must repeat that check at the point of filesystem use to cover changes
after this response.

The encoded `get` result is bounded by the runtime's 16,384-byte response
limit. A full update that would exceed it is rejected before persistence.
If a caller times out while the state service is persisting an update, the write
may still have committed; read the current revision before retrying.

`update` is a full replacement. `expected_revision` compares to the document
revision; the provider increments it once and writes with the state namespace's
revision as `expected_revision` in the state CAS. After that durable write,
the provider sends the GET-shaped configuration to diagnostics as
`apply_host_logging`. Success requires diagnostics to confirm the same applied
revision, after opening the requested logging sink:

```json
{"ok":true,"ready":true,"version":1,"revision":1,
 "values":{"file_state":"sunshine_state.json",
   "vibeshine_file_state":"vibeshine_state.json","credentials_file":"",
   "pkey":"credentials/cakey.pem","cert":"credentials/cacert.pem",
   "log_path":"amaterasu.log","min_log_level":"product_default"},
 "effective":{"file_state":"C:\\ProgramData\\...\\sunshine_state.json",
   "vibeshine_file_state":"C:\\ProgramData\\...\\vibeshine_state.json",
   "credentials_file":"C:\\ProgramData\\...\\sunshine_state.json",
   "pkey":"C:\\ProgramData\\...\\credentials\\cakey.pem",
   "cert":"C:\\ProgramData\\...\\credentials\\cacert.pem",
   "log_path":"C:\\ProgramData\\...\\amaterasu.log",
   "min_log_level":"info"},
 "logging":{"saved_revision":1,"applied_revision":1,"status":"applied"}}
```

An explicit diagnostics rejection leaves its prior logger active. The provider
then restores the previous settings through another state CAS, using the next
document revision to keep revisions monotonic. A confirmed compensation reports
`logging_apply_failed` with `logging.status: "rolled_back"`. If the
compensating write is explicitly rejected, it reports `logging_rollback_failed`
with `logging.status: "saved_not_applied"`. An ambiguous compensation reports
`logging_rollback_unconfirmed`. When a write or diagnostics apply
has an ambiguous transport or timeout outcome, the provider does not claim
success or issue a speculative rollback. It reports `save_unconfirmed` or
`logging_application_unconfirmed`, including the attempted or saved revision.
The caller should GET the configuration and inspect diagnostics status before
retrying. A signed deployment must grant this provider access to
`amaterasu.diagnostics/1`; an explicit grant denial is compensated like any
other definite apply failure.

Document revision conflicts return `revision_conflict` and
`error.current_revision`. State unavailability, malformed responses, storage
failures, and path validation failures fail closed. Legacy source migration,
live migration execution, host/logger consumers, and management UI wiring must
be qualified by integration before these values are treated as product-wide
parity.
