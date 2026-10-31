# Extension registry

The managed service exposes `list`, `describe`, `capabilities`,
`describe_action`, `settings_get`, `settings_update`, and `settings_migrate`.
The operator supplies installed descriptor policy in this module's bootstrap
settings. Component signatures do not authenticate bootstrap settings.

Each entry contains `descriptor`, `descriptor_sha256` (lowercase SHA-256 of
its deterministic `serde_json` serialization), `approved_methods` (complete
broker `ApiMethod` schemas), `grants` (caller, exact scope, read/write), and an
optional `migrations` array. Runtime broker discovery intersects the active
parent's caller and initiator permissions; an installed descriptor is only
visible if the selected provider and complete method schema also match the
operator policy. Both caller and initiator need exact scope grants. The registry
does not derive authority from a payload selector.

Settings use complete scope replacement and required document revisions.
Every represented application/client/session combination, including absent
scopes, is validated by the provider before the state-service CAS. Validation is
bounded to 64 combinations and fails closed beyond that budget. Migration plans
are installed policy and must explicitly rename, remove, or replace changed
values. `write_outcome_unknown` requires reloading before any further update.

Dynamic application and session reads may include a `target` (tagged
`LifecycleTarget`) and optional `input_binding`. The registry requests lifecycle
authorization for every selected scope, relaying only the caller and initiator
provided by the broker. It accepts only the coordinator's matching canonical
target and scope. A read is authorized when lifecycle accepts its scope request;
revocation does not retroactively retract an already admitted read. Input effects
require separate revalidation. Input-token authorization requires a configured trusted input
consumer and a current token bound to the exact active stream. Dynamic paired
client reads and dynamic writes remain unavailable. Provider validation and
migration have not yet been qualified through an installed host fixture.

For example, the following bootstrap settings pin one minimal provider. Its
`settings_validate` method must also be installed and registered with an
identical broker approval; add the relevant broker call grants for the registry
and the initiating modules. The hash corresponds to the exact descriptor shown.

```json
{
  "descriptors": [{
    "descriptor": {
      "provider": "example.demo",
      "revision": 1,
      "options": [],
      "actions": [],
      "capabilities": [],
      "settings_validate": {
        "service": "example.demo/1",
        "method": "settings_validate",
        "interface_major": 1,
        "interface_minor": 0,
        "schema_revision": 1
      }
    },
    "descriptor_sha256": "b26f4d1080cf077a280b9f888307a37a6b3e35aad9a20925077009feebd2cede",
    "approved_methods": [{
      "service": "example.demo/1",
      "method": "settings_validate",
      "interface_major": 1,
      "interface_minor": 0,
      "request": {"required": {"values": "object"}, "optional": {}, "allow_extra": false},
      "response": {"required": {"ok": "boolean", "valid": "boolean"}, "optional": {}, "allow_extra": false},
      "privilege": "user"
    }],
    "grants": [{
      "caller": "amaterasu.management",
      "scope": {"kind": "host"},
      "read": true,
      "write": true
    }],
    "migrations": []
  }]
}
```

The provider's pure validation request is `{"values": {...}}`; a successful
response is `{"ok": true, "valid": true}`. Any other result rejects the update.
