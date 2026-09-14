# Native service interfaces

Native factories provide an optional in-process C ABI data path for independently
built libraries. Frames and packets travel through product-owned typed tables and
buffers; JSON carries only registration/binding/release control descriptors. The
shell has no media tables, feature dispatch, or new entry points.

Native modules are trusted code. Pointer validation checks layout and authority;
it cannot prove arbitrary native addresses are mapped or make a module a sandbox.
Only explicitly `native_provide`-granted principals may register factory addresses,
and only `native_use`-granted principals may bind the corresponding service name.
Ordinary `provide` and `call` grants confer neither permission. Both native grant
sets default empty, validate names/counts, and intersect with policy independently.
Never grant native pointer operations to an external console, browser, or network
proxy that forwards caller-chosen control messages.

## ABI

All tables are `repr(C)`, with native-sized pointers, C calling convention, no Rust
strings, references, trait objects, allocator ownership, or unwinding across calls.
The actual Rust declarations are in `amaterasu-abi`:

```text
NativeInterface {
    header: AbiHeader,             // complete product table size and interface version
    context: void*,                // per-consumer provider-owned state
    release: nullable C fn(void*)
}
NativeInterfaceQuery {
    header: AbiHeader,             // generic Amaterasu ABI version
    interface_major: u32,
    min_minor: u32,
    min_size: u32
}
NativeServiceFactory {
    header: AbiHeader,             // generic Amaterasu ABI version
    context: void*,
    bind: nullable C fn(void*, BorrowedBytes authenticated_caller,
                        const NativeInterfaceQuery*, const NativeInterface**) -> Status
}
```

A product table embeds `NativeInterface` first and appends typed callbacks. Its
header describes its full size and product-specific version, not necessarily the
bootstrap ABI version. Versioned service names also identify the product protocol.
The consumer requests exact major, minimum minor and minimum complete table size.
The runtime checks alignment, prefix size, release callback, versions, and size
bounds before handing it to the consumer. `AbiHeader::supports<T>()` applies to
bootstrap/factory/query versions; product interfaces may define a different major.

The runtime copies the factory descriptor during registration. The provider's
context and callback code remain valid until all consumers stop. The provider must copy the borrowed authenticated principal
into each binding context if needed after `bind` returns. The runtime obtains this
principal from its immutable endpoint; no JSON field can select it. Product methods
use that bound context to authorize their stream/session IDs. This authenticates
the immediate module consumer; it does not automatically delegate a JSON request's
initiator or make all objects accessible to that module.

Factory callbacks write null on failure and transfer one live interface on success.
The returned table, context and functions remain valid until exactly one
`release(context)`; release may free the entire table, using the provider's own
allocator. No consumer frees provider memory. All callbacks must be bounded,
synchronize shared state as required, and contain panics. Frame/packet borrowing,
retention, buffer release, cancellation and thread rules belong to each product ABI.
A compatible prefix returned with the wrong requested version is released by the
runtime. A malformed/truncated native table violates the provider contract and
cannot necessarily be released safely.

## SDK and control path

Provider initialization uses unsafe `RuntimeClient::register_native(service,
&factory)`. The descriptor may move after registration; any context it references must
retain a stable address and remain alive until consumers stop. Consumers call unsafe
`bind_native(service, major, min_minor, min_size)` to obtain `NativeBinding`.
`binding.context()` returns the bound context and unsafe `binding.table::<T>()`
checks size/alignment before accessing the exact product table promised by the
caller. Do not retain references, callbacks or asynchronous operations beyond the
binding. `NativeBinding` deliberately does not implement Send/Sync.

Binding drop issues `NativeRelease` through the original endpoint. The runtime
checks the binding owner, removes tracking exactly once, and calls the provider's
release callback outside all broker/registry locks. Bind callbacks likewise run
outside those locks and may reenter runtime operations. The runtime reserves a
binding slot before calling the provider, preventing unbounded concurrent creation.
Metadata is bounded to 256 immutable factories and 1024 live or pending bindings;
release recovers capacity. There is no automatic retry/replay of a failed bind.

Internal control envelopes are `native_register {service,factory}`, `native_bind
{service,interface_major,min_minor,min_size}`, and `native_release {binding}`;
`native_bound {binding,address}` is the binding response. Addresses are process-local
control descriptors, not portable handles or serialized media. The safe SDK `send`
rejects these commands; only its explicitly unsafe native API submits them. Raw
`HostApi::call` is already unsafe. Direct pure `Broker::dispatch` does not handle
native pointers; native commands require the authenticated FFI endpoint.

## Lifecycle and limits

The signed package must start providers before consumers and destroy consumers
before providers. Stop/join consumer work and release all child buffers/objects
before dropping bindings. The runtime must remain alive throughout that sequence.
Registration is immutable for the process lifetime; live unregister/replacement,
cross-process handles and hot provider swapping are not implemented. Runtime
destruction does not call saved pointers after module DLLs may have been unloaded;
leaking a binding is a lifecycle bug, not an automatic recovery mechanism.

Tests cover grant separation before pointer reads, authenticated principal binding,
release ownership/exactly-once behavior, callback lock freedom, short factory
headers, incompatible table cleanup and control-message validation. Hardware
buffer interoperability and product callback behavior require their own tests.

