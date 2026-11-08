# Configuration library

`amaterasu.configuration/1` owns per-caller schemas and validated values. It uses
`amaterasu.state/1` for persistence and has no filesystem or cryptographic code.
State must start first. Configuration needs a state call grant; originating callers
also need that grant because delegated requests preserve the parent's authority.

Payloads use `action`:

```json
{"action":"register","fields":{"enabled":{"type":"boolean","default":true},"count":{"type":"integer","default":2,"minimum":1,"maximum":10},"title":{"type":"string","default":"Hello","max_length":100},"scale":{"type":"decimal","default":1.25,"minimum":0.5,"maximum":2.0},"quality":{"type":"enum","default":"balanced","options":["fast","balanced","quality"]},"tags":{"type":"list","default":["gaming"],"item":{"type":"string","default":"","max_length":64},"max_items":8},"display":{"type":"object","default":{"enabled":true},"fields":{"enabled":{"type":"boolean","default":true}}}}}
{"action":"get"}
{"action":"update","values":{"count":3},"expected_revision":1}
```

Successful responses are `{"ok":true,"revision":1,"fields":{...},"values":{...}}`.
Errors are `{"ok":false,"error":{"code":"...","message":"..."}}`; revision
conflicts also include `error.current_revision`. Revisions belong to each caller's
configuration, independently of the state namespace revision used for persistence.
Only the runtime-supplied caller identity selects the configuration; payloads cannot
supply another identity. Unknown operation or definition fields are rejected.

Registration persists defaults at revision 1. Re-registering an identical schema
returns existing values without writing. Incompatible re-registration fails with
`schema_conflict`. Updates merge only known, valid fields and advance the revision;
`expected_revision` is optional. Integer fields accept signed 64-bit integers.
Decimal fields accept finite JSON numbers and may set inclusive `minimum` and
`maximum` bounds. Enum fields hold one string from a nonempty, unique `options`
array. List fields hold arrays whose elements match the nested `item` definition;
`min_items` defaults to zero and `max_items` defaults to 4096. Object fields hold a
complete object matching their nested `fields`: all declared child fields are
required and unknown child keys are rejected. The example shows each definition's
required `default`.

String lengths and enum option lengths count Unicode scalar values. String fields
default to a maximum length of 4096, and explicit bounds cannot exceed it. Enum
options and lists are limited to 4096 entries. Schemas have at most 128 fields
across the full nested schema and at most 16 levels of nesting. Entire stored
documents are limited to 12,000 serialized bytes, including escaping and schema
defaults.

Each tick advances one asynchronous state operation, polls at most one incoming
request, and starts at most one queued operation. There are at most 32 queued
requests plus one active operation. State loads and saves carry the original parent
and remaining deadline. A mutation is acknowledged only after state confirms the
write. Cancelled or expired caller completions are discarded.


## Declarative schema migrations

Use `migrate` to replace the complete schema at a known configuration revision:

```json
{"action":"migrate","expected_revision":2,"fields":{"active":{"type":"boolean","default":false},"title":{"type":"string","default":"Hello","max_length":200},"quality":{"type":"integer","default":5,"minimum":1,"maximum":10}},"renames":{"enabled":"active"},"removed_fields":["count"]}
```

`expected_revision` and `fields` are required. `renames`, `removed_fields`, and
`values` are optional (defaulting to empty). The migration preserves existing
values under unchanged names, applies renames simultaneously, and initializes
new fields from validated defaults. The example preserves `enabled` as `active`,
preserves `title`, explicitly discards `count`, and initializes `quality`.

Every old field must be preserved, renamed, or explicitly listed in
`removed_fields`. Two old fields cannot map to one destination; rename sources
must exist and destinations must appear in the replacement schema. A field cannot
be both renamed and removed. Renames can swap names. Listing an old name in
`removed_fields` explicitly discards its value; if the replacement schema still
contains that name, it resets to its new default unless supplied in `values`.
Unknown or duplicate removal names are rejected.

Optional `values` explicitly replaces resulting values, using names from the new
schema. This permits intentional type/bounds changes without implicit coercion:
for example, change an integer field to a string and supply `"values":{"count":"7"}`.
All final values and all schema defaults must validate. Invalid mappings return
`invalid_migration`; invalid schemas or values return `invalid_schema` or
`invalid_values`. Failures leave the prior persisted document intact. A successful
migration advances the configuration revision once and persists fields and values
in one state CAS write. This is a declarative operation, with no script execution,
automatic product-schema inventory, or schema-version inference.

Existing documents remain readable without conversion. `register`, `get`, and
`update` retain their previous request and response forms. Repeating a completed
migration with its old expected revision fails, so reload after an ambiguous
mutation timeout rather than blindly retrying.

The caller-scoped `product_get` operation only reads the existing product
document; it does not create, migrate, or update it. `product_update` performs
the corresponding revision-checked mutation.

## Change notifications

The module needs a publish grant for `amaterasu.configuration.changed/1`.
Subscribers need the corresponding event subscription grant. After state confirms
a successful registration, update, or migration, configuration publishes:

```json
{"owner":"caller.module","revision":3}
```

`owner` is the authenticated immediate runtime caller, not the original initiator
or a request field. Events contain no configuration values, defaults, or field
names. Topic subscribers can observe owner/revision metadata across callers, but
`get` remains caller-scoped. Reads, identical re-registration, rejected mutations,
and failed or cancelled state acknowledgements publish no event.

Notifications are best effort and are not a durable change log. A publication
failure never rolls back a persisted mutation or changes its successful response.
If state confirms the commit but the parent expires before completion, the event
is still attempted. If cancellation hides an acknowledgement, the write may have
committed without an event. Subscribers must use `get` as authoritative state:
subscribe before the initial read, compare document revisions, ignore old events,
and resync after revision gaps, event loss/queue overflow, reconnect, or restart.
There is no event replay or exactly-once delivery guarantee. Consumers requiring
freshness despite a missed final event must periodically resync with `get`.
