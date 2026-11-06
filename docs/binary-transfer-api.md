# Binary transfer between plugins

The optional `TransferApi` v1 carries binary data directly between independently
built native libraries. The communication runtime authorizes a binding and the
provider checks ownership of each channel. Payload bytes do not pass through
JSON, base64, broker queues, the broker's 16 KiB payload limit, or the host tick.
The shell has no image, codec, network endpoint, socket or packetization logic.

This is an implemented API and reusable SDK, with a separately compiled test
provider. **Existing Steam/Lutris artwork and the production video/network paths
have not been migrated.** The API test is not an installed artwork performance
fix, a PyroWave implementation, or evidence of physical network throughput.

## Why the boundary is needed

Artwork uses the shared binary-transfer SDK; see [artwork transfer](artwork-transfer.md). The existing media
interfaces already use native buffers, but the legacy packet validation and
transport negotiation still restrict codecs, and the generic encoder output
has its own 16 MiB limit. Increasing the JSON limit would neither remove the
per-request scheduling cost nor provide buffer ownership and backpressure.

PyroWave is a useful behavioral reference, not a new core codec enum. Its
[upstream design](https://github.com/Themaister/pyrowave) targets high-bandwidth,
low-latency GPU encoding. The inspected Vibeshine `docs/pyrowave-protocol.md` and
`src/stream.cpp` distinguish large encoded frames from their RTP/FEC shards,
including a critical prefix, record boundaries, wire budgets and pacing.
Amaterasu must allow producer and transport plugins to agree those semantics
without embedding PyroWave policy in the shell or generic byte API. No source
was copied or translated from Vibeshine.

## Implemented contract

| Capability | v1 behavior |
|---|---|
| Version negotiation | Native major/minor/table size plus versioned description; incompatible tables fail explicitly |
| Formats | Open namespace/value/version IDs for payload and metadata; exact agreement before open; no codec enum in the data path |
| Transfer sizing | Negotiated chunk/queue/record limits; a record may span many bounded chunks; no 8 KiB, 1 MiB or 16 MiB payload ceiling imposed by the API |
| Buffer layout | Up to 256 negotiated immutable CPU segments per chunk; checked lengths and offsets |
| Owned producer data | SDK moves boxed storage into the local queue without copying payload |
| Native input | Borrowed input copies once, or retained input holds a producer-owned C lease without copying payload |
| Native output | Synchronous borrowed segments, no payload copy by the SDK; receiver may synchronously forward or copy into its own bounded storage |
| Backpressure | All-or-nothing local admission; BUSY changes no ordering state, copies/retains nothing, and preserves caller ownership |
| Receive rejection | Non-OK keeps the same front chunk for retry; partial external effects must be staged by the consumer |
| Scheduling | Generation-based condition-variable waits; no broker request or module tick between chunks |
| Memory | Per-channel bytes/chunks and provider-wide reservations; in-flight receives and retained backing allocations remain charged |
| Isolation | Runtime-attested principal, per-channel owner, one binding claim, random provider incarnation, monotonically allocated IDs |
| Completion | Finish rejects incomplete logical records and drains accepted data; empty means local completion, not peer receipt |
| Cancellation | Wake waiters, reject new work, retire pending buffers and in-flight callbacks synchronously |
| Language ABI | C-compatible structs/callbacks and matching C/C++ header; no Rust allocator/collections cross the boundary |

`Schema` IDs must be agreed through the provider's authorized control service.
The SDK intersects numeric limits using `negotiate_limits`; it does not infer
format compatibility. Providers must reject unknown required metadata schemas.
Payload and metadata schemas need nonzero namespace, value and version fields;
use a negotiated empty-metadata schema when there is no side data.

Limits are policy, not promises to allocate their maximum immediately. The hub
reserves each channel's full `max_queued_bytes` against its global budget at
creation, so opening many empty channels cannot overcommit the provider.
Closing returns the budget only after retirement. Chunk/segment limits also
bound descriptor overhead, which is separate from charged payload/metadata.
Owned byte and segment arrays use boxed slices, so spare vector capacity is not
silently pinned. Foreign leases declare the complete pinned payload backing
storage, including unused capacity; misreporting it violates the trusted ABI.

Record IDs strictly increase between records. Within a record, fragments have
contiguous offsets and an invariant total length. Metadata appears only on the
first fragment. This supports both a whole small asset in one callback and a
very large logical frame delivered in bounded pieces. It is an ordered channel,
not an unordered packet scheduler. Use separate channels when streams need
independent admission, priorities or ordering.

## Ownership and lifecycle

The public contract is in `crates/data-plane`, the SDK in
`amaterasu_module_sdk::data_plane`, and the C/C++ header in
`sdk/include/amaterasu_data_plane.h`.

1. Provider startup creates a `Hub` with a channel count and aggregate byte budget,
   retains its boxed `NativeFactory`, and registers that factory under its signed
   native service with `register_transfer` (or the `native_register_typed`
   command and `AMATERASU_DATA_TRANSFER_CONTRACT` in C). Consumers use typed
   binding with the same family. Untyped factories are rejected for all aliases;
   version and table size alone do not identify the interface. Package dependencies must order consumers before providers at
   destruction; call and native-use grants remain separately explicit.
2. An authenticated control request authorizes a domain resource, its exact
   revision/generation, negotiated schemas/limits and the permitted consumer.
   Only then does the provider call `Hub::create`. An ID never grants access by
   possession. Do not take the owner, service name, native address or filesystem
   path from an untrusted client payload.
3. Return the opaque channel ID and complete description through control.
   `TransferClient::bind` compares native describe with that exact negotiated
   result. Runtime native authorization is direct principal authorization, not
   inherited broker-parent authority: the control service must check the
   initiating request and explicitly authorize any delegated data consumer.
   Do not expose these internal u64 IDs as browser JavaScript numbers.
4. A SOURCE channel permits native receive; a SINK permits native submit/finish.
   The provider has the opposite local SDK handle. Each channel is claimed by
   one native binding; another binding, even under the same principal, cannot
   steal it. IDs are not recycled after cancellation or restart.
5. `Channel::push` moves locally owned data; `TransferClient::submit_owned`
   supplies retained input across a DLL boundary with producer-local allocation
   and release callbacks. Rejection returns the original owned chunk. `submit`
   provides a copying alternative for a temporary borrow.
6. A received view is valid only during its sink callback. No returned pointer
   may outlive that callback. Receive never exposes a retainable output lease in
   v1. Input lease callbacks must be thread-safe, nonblocking and infallible;
   they must not reenter transfer operations or unwind across C.
7. Native API calls may run concurrently on dedicated workers. The safe client
   wrapper follows `NativeBinding`'s module-thread confinement. A custom worker
   wrapper must hold the binding live and join before release. `wait` is a
   blocking worker operation with a bounded timeout, never a module-tick call.
   It checks generation under the same lock as the wait, preventing lost wakeups.
8. Finish stops admission and allows queued chunks to drain. Cancel waits for
   in-flight sinks and lease release callbacks. Never reenter receive/cancel/
   release for the same channel from its sink callback. Drop releases all claimed
   channels through a synchronous barrier; failed teardown must not allow DLL
   unload. Providers additionally stop and join their own workers.
9. Domain cancellation, timeout, revision invalidation and authorization
   revocation call `Hub::close`. Providers must also expire unclaimed channels
   after lost/cancelled control replies. A native binding is not a session token;
   automatic expiry and account revocation are domain responsibilities.

Native modules remain trusted code. Grants and pointer checks are routing and
correctness controls, not a sandbox. Leased storage is CPU-readable memory;
GPU resources, GPU completion fences and OS socket/handle ownership continue to
use separately negotiated domain contracts.

## Consumer migration and acceptance gates

The following are required migrations, not behavior provided by the fixture:

| Consumer | Required use of the API | Acceptance evidence |
|---|---|---|
| Steam, Lutris, external library providers | Authorize one immutable artwork snapshot; return generic transfer metadata; pin provider/revision/MIME; close on reply loss/timeout/revocation | Real signed providers serve the same full image with one control setup and bounded binary transfer, without per-chunk broker calls |
| Library facade | Bind only startup-selected providers; preserve initiating authority/export policy; keep provider references opaque | Unknown/unauthorized routes rejected; swapped revisions and stale handles cannot return mixed artwork |
| Management web API | Authenticate one image request, stream binary HTTP response with bounded pending output, cancel on disconnect; cache by provider/revision and authorization scope | Signed-browser trace and local/remote timings; no base64 or repeated HTTP POST per 8 KiB; memory bounded with a slow browser |
| Moonlight artwork | Consume the same binary asset API; schedule bounded concurrency and cache complete authorized revisions | Real client artwork load, rejection/disconnect cleanup and concurrent request behavior |
| Video/encoder orchestration | Negotiate producer/transport format and metadata IDs rather than forcing the legacy codec enum; adapt existing output with explicit limits | Independently built nonstandard-format producer reaches chosen transport; no accidental fallback or reinterpretation |
| Codec/framing transport | Consume whole or fragmented records; define timing, critical-region/FEC hints, deadlines and wire budgets in its negotiated metadata schema | PyroWave and another unfamiliar format work without shell/data-ABI changes; malformed/unsupported schemas rejected |
| Packet-delivery plugin | Accept bounded batches of datagrams/segments with authorized destination/session and completion semantics; own socket I/O and OS batching | Loopback send/receive, slow receiver and loss/cancellation tests; actual remote throughput/latency before performance claims |

The logical record is **not** an oversized UDP datagram. MTU selection,
packetization, FEC, encryption/nonce allocation, pacing, deadlines, congestion
policy and delivery acknowledgments belong in their respective negotiated
plugins. A packet-delivery profile can use segments and metadata for a batch;
it must define atomic local admission and partial OS-send retry without
pretending local queue success proves transmission. Reliable assets and
latency-sensitive frames need different policies over the same primitive.

The broker JSON limit remains unchanged. Raising it, removing all budgets,
putting codecs into the generic runtime, or attaching unbounded socket buffers
to it are not migration strategies.

## Validation

Run the ordinary SDK/runtime tests and the independent library qualification:

```sh
cargo test --offline --locked -p amaterasu-data-plane -p amaterasu-module-sdk -p amaterasu-runtime -p amaterasu-data-plane-fixture
python scripts/test_binary_transfer_api.py
cc -std=c11 -Wall -Wextra -Werror -I sdk/include scripts/check_data_plane_abi.c -o /tmp/amaterasu-data-abi
/tmp/amaterasu-data-abi
c++ -x c++ -std=c++17 -Wall -Wextra -Werror -I sdk/include scripts/check_data_plane_abi.c -o /tmp/amaterasu-data-abi-cpp
/tmp/amaterasu-data-abi-cpp
```

The independent test loads a separately compiled cdylib through the real module
and runtime ABI. It transfers 985,696 bytes and 32 MiB + 17 bytes in one native
callback each, uses a nonstandard schema, validates every byte, submits repeated
32 MiB retained buffers, checks rejection ownership, and retires queued producer
storage before unloading. The timed retained admission is a queue operation,
not a network benchmark. The fixture does not encode/decode images or video,
open sockets, implement PyroWave, or replace the installed host's artwork route.
