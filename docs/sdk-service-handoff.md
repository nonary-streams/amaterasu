# SDK service integration handoff

The SDK provides typed broker service calls and event subscriptions, plus
caller-scoped State and product settings adapters. Trusted startup composition
selects providers against declared contracts, versions, capabilities, grants,
and dependencies. Selected native interfaces require typed binding; API-approved
routes cannot also be declared as raw services. These grants constrain broker
operations; native modules remain trusted code.

Event recovery fences delivery after overflow or a potentially consumed malformed
or uncertain poll. Explicit close and recovery retain failed cleanup handles for
retry; dropping a subscription attempts cleanup only once. A lost subscribe
admission reply can leave an admitted subscription without a caller-held handle.
Cancellation does not undo completed work.

## Validation

At code commit `a259421a7a82b527763203005a76a8418d647eca`:

- The combined contracts, package, runtime, module SDK, State and configuration
  test run passed 350 tests, with eight artifact-dependent tests ignored.
- Both opt-in tests described in
  [State SDK artifact conformance](../fixtures/sdk-service-conformance/README.md)
  passed with freshly built artifacts. The unchanged consumer SHA-256 was
  `cb3946fd4e75c7e36525b8f66a460990abe9e1a0a3e74ad4b5072c4bbc79d438`.
- Workspace and fixture formatting checks passed. Clippy with `-D warnings`
  passed for the affected packages, host, release and fixture workspace.
- A separate capture-authority all-targets check fails in the pre-existing
  `proxy_tests.rs` frame initializer, which omits `color`, `guard_count`, and
  `hdr_metadata`; the initializer and descriptor are unchanged from the base.

The replacement fixture implements a bounded, memory-only key/value subset, not
the complete State service or persistent replacement storage. Signed contract and
capability claims do not prove implementation behavior or native ABI negotiation.
