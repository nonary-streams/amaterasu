# Replaceable network providers

Plugins use `amaterasu_module_sdk::network::Network` for network operations. The
SDK calls a versioned native `NetworkApi` interface; it does not create an OS
socket or fall back to a built-in implementation when selection fails.

The default independently loaded component is `amaterasu.network` (library
`amaterasu_network`), implementing the service contract `amaterasu.network/1`.
Its socket implementation is in `crates/network-sockets`. Replacement providers
can implement the same contract or a distinct approved version-1 service name.
The consumer still uses the same SDK methods.

## Selecting a provider

Each consuming module's trusted startup settings accept:

```json
{
  "network_provider": "example.network/1"
}
```

Omitting the field selects `amaterasu.network/1`. A selected service must be a
valid versioned service name ending in `/1`. Startup binds that service and
checks interface major, minimum minor, table size and capability descriptor.
A missing service, rejected grant or incompatible table fails explicitly. The
runtime also checks the immutable `amaterasu.network-interface/1` family recorded
by `register_native_typed` against the consumer's `bind_native_typed` request,
before invoking the factory. An alias registered for another same-size/version
interface, or an untyped legacy factory, cannot be mistaken for networking.
A consumer should check its required features before admitting work; optional
features can fall back through other operations on the **same selected provider**.

A signed package composition must include the selected implementation, its
startup dependency and the consuming principal's native-use grant. Default
compositions grant native use of `amaterasu.network/1` and depend on the default
component. To select a replacement, change those explicit grants/dependencies
as well as the trusted setting. Selecting an alias in JSON never grants native
access or loads a library by itself. Default and replacement implementations
are separate identities from the versioned service interface they provide.

Provider startup settings currently accept `max_handles`, default 1024, between
1 and 65536. The budget applies across bindings. Consumer handle IDs belong to
one runtime-attested binding and cannot be used through another binding.

```rust,ignore
let network = Network::from_settings(&runtime, &startup_settings)?;
if !network.supports(network::abi::TCP | network::abi::DNS) {
    return Err("required networking capabilities are unavailable".into());
}
// Pass a clone into an owned worker for blocking outbound operations.
let stream = network.connect_tcp(address, Duration::from_secs(8))?;
```

The underlying native table and the public C/C++ declarations are in
`crates/network-api/src/lib.rs` and `sdk/include/amaterasu_network.h`.

## Contract and SDK operations

| Capability | Operations and semantics |
|---|---|
| TCP | Bind/listen, accept, connect with deadline, read/write/peek, addresses, shutdown, socket options |
| UDP | Bind/connect, send/receive whole datagrams and source addresses, multicast, TTL, broadcast |
| DNS | Bounded hostname length and result count, caller-supplied timeout |
| Interface addresses | Bounded IP-address/interface-index list, including IPv6 scope information |
| Batched datagrams | Optional bounded accepted-prefix submission; callers retry only the unaccepted tail |
| Ownership | Opaque binding-local IDs, provider-retained sockets, SDK shared resource guards |
| Error behavior | Versioned provider-neutral statuses mapped to `io::ErrorKind`; OS-specific errno remains inside the implementation |
| Limits | Negotiated maximum I/O size, handles and batch size; allocations/descriptor arrays bounded at the ABI |

TCP reads/writes clamp each call to the provider's maximum I/O size and report
partial progress. `Read::read_exact` and `Write::write_all` can therefore move a
larger logical buffer without increasing a fixed API ceiling. UDP operations
preserve datagram boundaries, including real zero-length datagrams; arbitrary
large frame transfer is a separate data-plane/packetization responsibility.

`TcpListener`, `TcpStream` and `UdpSocket` expose portable operations rather than
file descriptors or platform socket types. `try_clone` creates another local
SDK owner of the same opaque resource; the final resource guard closes it.
Consequently, nonblocking/timeouts and shutdown act on the shared underlying
resource. Provider table release follows all network/resource guards.

Default listener options preserve separate IPv4/IPv6 listeners: IPv6-only is
true, nonblocking is true, Unix address reuse is enabled without port sharing,
and backlog is 4096. Call `bind_tcp_with_options` with `TcpListenOptions` for an
explicit backlog or blocking listener. UDP binds are nonblocking by default.
Accepted TCP streams are nonblocking, including those accepted from an explicitly
blocking listener; a blocking worker must explicitly disable nonblocking on its
accepted stream. Outbound TCP connect returns a blocking stream with an explicit connect timeout;
HTTP/UPnP workers set their read/write timeouts and must use their own joined
worker lifecycle, never block a module tick.

BATCH is optional. Its `max_batch` is independent of a consumer's preferred batch
size. A valid accepted prefix is successful local admission; resubmitting that
prefix would duplicate datagrams. The default provider keeps Linux UDP_SEGMENT
acceleration where the datagram shape permits it and uses provider-local portable
sends otherwise. Unsupported acceleration must not cause the consumer to create
its own socket.

## HTTP and protocol layering

The optional `network-http` SDK feature adapts ureq HTTP parsing/TLS over the
selected network provider's DNS and TCP operations. The adapter supplies both
connector and resolver; it does not leave a default OS connector or resolver in
the chain. Steam artwork and updater workers retain the selected provider through
their joined lifetime. HTTP redirects, certificate verification, byte limits,
request origins and application authorization remain explicit domain policies.

Likewise, NVHTTP, RTSP, management HTTP, GameStream control framing, packetization,
FEC, encryption and pacing continue to own their protocol state. They use opaque
provider operations for the delivery mechanism. A replacement provider must honor
the required TCP/UDP semantics for that consumer; it cannot substitute a different
protocol while silently claiming compatibility. New delivery features or protocols
need a negotiated extension or new interface version, not a concrete socket type
leaking into consumers.

## Threading and destruction

`HostApi` endpoints and the unsafe `RuntimeClient::from_endpoint` constructor
explicitly require a thread-safe runtime invoke callback/context. Principal
identity remains attached to the endpoint across all calling OS threads.
`Network::bind` additionally requires the selected service to implement the exact
thread-safe network table and requires its code/runtime to outlive every guard.
The safe `from_settings` path uses those established module/runtime contracts.
`Network` and its socket wrappers can therefore be cloned into provider-authorized
consumer workers. Last-guard release may run on that worker.

Provider native callbacks cannot unwind. Bytes are synchronous borrows; no Rust
allocator, collection, reference or socket crosses the table. Concurrent methods
retain the socket resource while operating and do not hold the registry lock
during network I/O. Provider bindings close all remaining handles on release.
Consumers must stop/join their workers and release network resources before the
provider module or communication runtime is unloaded.

The default provider runs OS name resolution in its own embedded helper
executable, with at most eight owned lookups. It does not leave `getaddrinfo`
threads executing in an unloading provider library. A request deadline cancels
its helper; the provider retains the exact process and containment ownership
until exit and reaping are observed. Cancellation alone does not establish
retirement. Shutdown announcement preserves the earliest deadline, and
quiescence remains pending or reports failure while any lookup is unretired.

The production host retains the complete loaded graph when quiescence fails,
including module instances, libraries and runtime endpoints, and refuses another
graph in that process. It returns a shutdown error and exits the worker instead
of invoking an unfinished module's destructor or acknowledging success.
Standalone factory users must likewise finish owned shutdown before destruction;
the owned shutdown operation returns the factory on pending or failed cleanup.
Its final destruction fallback is bounded by the remaining shutdown allowance
and aborts the embedding process if exact helper retirement remains unproven.
It never abandons an unreaped child or waits without a deadline.

The Network component obtains its helper storage root through the explicit
`amaterasu.host-state-context/1` grant. The host chooses that root; component
settings cannot replace it. Helper bytes are part of the signed provider image,
and launching does not search an ambient executable path. Existing approved
policies must include the new grant before selecting this component version;
a missing grant fails closed and does not authorize a policy bypass. Source
and portable tests do not establish native Windows or macOS qualification.

`Network::from_native_table` is an explicitly unsafe owned-interface constructor
for provider composition and tests. It releases a complete valid interface prefix
on an incompatible major/tail and releases the table on failed capability
negotiation. An unreadable or truncated pointer violates the native contract and
cannot safely be traversed for cleanup.

Native modules are trusted code. Grants route interfaces; they are not a native
code sandbox or an OS firewall.

## Replacement acceptance and tests

The optional `network-test-support` SDK feature exposes `Network::test_provider()`
for explicit default-provider injection in tests. Consumer dev-dependencies opt
into it; production selection never invokes it after a binding failure.
Alternative-provider tests can construct their own C table with
`Network::from_native_table`, or register it through real authenticated runtime
endpoints. A useful substitution test records/replies to the consumer's ordinary
SDK calls without opening an OS socket. A provider missing a required capability
must fail before work is admitted.

Run focused checks:

```sh
cargo test --offline --locked -p amaterasu-network-api -p amaterasu-network-sockets
cargo test --offline --locked -p amaterasu-module-sdk --features network-test-support,network-http network
cargo clippy --offline --locked -p amaterasu-network-api -p amaterasu-network-sockets -p amaterasu-network -p amaterasu-module-sdk --all-targets --features amaterasu-module-sdk/network-test-support,amaterasu-module-sdk/network-http -- -D warnings
```

Default socket tests bind only loopback addresses and ephemeral ports. They cover
TCP read/write, UDP and batching, resource budgets, per-binding ownership, final
release and zero-length datagrams. Source/test qualification is separate from
installation, real-client playback, physical-network throughput, Windows socket
offload validation and live provider replacement during an active stream.

## Optional receive diagnostics (v1.1)

The original `NetworkApi` remains a v1.0 prefix. The default provider also offers
`NetworkApiV1_1` under the same immutable interface identity. Consumers check the
`DATAGRAM_DIAGNOSTICS` capability, minor version and complete tail size before
using `recv_from_timed` or `socket_diagnostics`. The SDK uses the **selected v1
provider's** ordinary receive call when the tail is absent and returns unknown
timing; it never substitutes the default provider. All handles and allocations
remain owned by that binding.

Linux enables software receive timestamps before binding. `recv_from_timed`
returns kernel timestamp-to-userspace age when `KERNEL_RESIDENCE_REALTIME` is set.
This observation uses the kernel's realtime timestamp and a userspace realtime
sample; it includes kernel queue residence and return overhead. It is **not** a
client-to-host network measurement. Negative/invalid or missing timestamps are
unknown. A realtime clock step can affect this observation; the host's monotonic
spike timestamps and OS sample clock correlation are independent.

`SOCKET_MEMORY` establishes receive/send queue memory charges, buffer limits and
cumulative receive drops. Memory charges include OS packet overhead, and are not
payload byte counts. Other platforms or replacement providers may expose the
tail with flags zero; consumers retain unknown values. No IO outcome depends on
diagnostic availability. See [Linux socket timestamp semantics](https://man7.org/linux/man-pages/man7/socket.7.html).
