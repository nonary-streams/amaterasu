# Service guard: an example layer

This module is stacked on other modules' services. For each service named in
its settings it applies a per-caller rule on top of the broker's grants,
declines the callers the rule excludes, and passes every other call on
unchanged. The guarded services' consumers and providers are not rebuilt and
do not know the guard is there.

It shows the layer helpers in `amaterasu_module_sdk::requests`: `wrap`,
`Relay`, `refuse` and a forwarded direct call. `tests/guard.rs` runs it in
front of a provider through the module test kit. See
[stacking and extension points](../../../docs/stacking-and-extension-points.md).

## Settings

```json
{
  "services": {
    "example.notes/1": { "allow": [], "deny": ["example.untrusted"] }
  }
}
```

A caller in `deny` never passes. When `allow` is not empty, only the callers
it names pass. An empty rule passes every caller the broker admits, which
leaves the guard logging calls without restricting them.

## Policy

Nothing in the module's own files makes it a layer. Trusted policy does:
`policy-example.json` is the fragment to merge into the bootstrap policy.

- `principals` grants the guard a `call` on each service, which it needs to
  pass a request on.
- `service_layers` lists it on each service. Layers are listed outermost
  first.
- `settings` carries the rules.

`component-linux.json` is the component template for
`amaterasu-release pack-linux-session ... --external-component`. Its requested
grants must match the policy; packaging never widens policy.

A layer acts with the authority of every caller of the service it wraps: a
forwarded request keeps its caller. Approve one the way you would approve a
replacement provider for that service. The module is trusted native code, and
grants are not a sandbox.

A package that lists a layer and omits its component fails verification. A
runtime whose layer has not started refuses calls to the wrapped service as
`unavailable`; the layer is never skipped.
