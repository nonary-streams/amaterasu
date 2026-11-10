#ifndef AMATERASU_NETWORK_H
#define AMATERASU_NETWORK_H

#include "amaterasu.h"
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Provider-neutral native network ABI. Neither operating-system socket handles
 * nor Rust allocations cross this interface. Default service selection may be
 * overridden by trusted startup settings; implementations share this contract. */
#define AMATERASU_NETWORK_CONTRACT "amaterasu.network-interface/1"
#define AMATERASU_NETWORK_INTERFACE_MAJOR 1u
#define AMATERASU_NETWORK_INTERFACE_MINOR 0u
#define AMATERASU_NETWORK_SERVICE "amaterasu.network/1"
#define AMATERASU_NETWORK_CAP_TCP UINT64_C(1)
#define AMATERASU_NETWORK_CAP_UDP UINT64_C(2)
#define AMATERASU_NETWORK_CAP_DNS UINT64_C(4)
#define AMATERASU_NETWORK_CAP_BATCH UINT64_C(8)
#define AMATERASU_NETWORK_CAP_MULTICAST UINT64_C(16)
#define AMATERASU_NETWORK_CAP_INTERFACES UINT64_C(32)
#define AMATERASU_NETWORK_MAX_ADDRESSES 256u
#define AMATERASU_NETWORK_MAX_BATCH 256u
#define AMATERASU_NETWORK_MAX_IO_BYTES (64u * 1024u * 1024u)

#define AMATERASU_NETWORK_WOULD_BLOCK 100
#define AMATERASU_NETWORK_TIMED_OUT 101
#define AMATERASU_NETWORK_INTERRUPTED 102
#define AMATERASU_NETWORK_CONNECTION_REFUSED 103
#define AMATERASU_NETWORK_CONNECTION_RESET 104
#define AMATERASU_NETWORK_CONNECTION_ABORTED 105
#define AMATERASU_NETWORK_NOT_CONNECTED 106
#define AMATERASU_NETWORK_ADDRESS_IN_USE 107
#define AMATERASU_NETWORK_ADDRESS_UNAVAILABLE 108
#define AMATERASU_NETWORK_PERMISSION_DENIED 109
#define AMATERASU_NETWORK_UNSUPPORTED 110
#define AMATERASU_NETWORK_NOT_FOUND 111
#define AMATERASU_NETWORK_BROKEN_PIPE 112
#define AMATERASU_NETWORK_INVALID_DATA 113
#define AMATERASU_NETWORK_RESOURCE_EXHAUSTED 114
#define AMATERASU_NETWORK_IO_ERROR 115
#define AMATERASU_NETWORK_HOST_UNREACHABLE 116
#define AMATERASU_NETWORK_NETWORK_UNREACHABLE 117

#define AMATERASU_NETWORK_OPTION_NONBLOCKING 1u
#define AMATERASU_NETWORK_OPTION_READ_TIMEOUT_MS 2u
#define AMATERASU_NETWORK_OPTION_WRITE_TIMEOUT_MS 3u
#define AMATERASU_NETWORK_OPTION_NODELAY 4u
#define AMATERASU_NETWORK_OPTION_BROADCAST 5u
#define AMATERASU_NETWORK_OPTION_TTL 6u
#define AMATERASU_NETWORK_OPTION_MULTICAST_IF_V6 7u
#define AMATERASU_NETWORK_OPTION_MULTICAST_HOPS_V6 8u
#define AMATERASU_NETWORK_OPTION_MULTICAST_LOOP_V4 9u
#define AMATERASU_NETWORK_OPTION_MULTICAST_LOOP_V6 10u
#define AMATERASU_NETWORK_OPTION_MULTICAST_TTL_V4 11u
#define AMATERASU_NETWORK_OPTION_IPV6_ONLY 12u
#define AMATERASU_NETWORK_OPTION_SOCKET_TOS 13u
#define AMATERASU_NETWORK_SHUTDOWN_READ 1u
#define AMATERASU_NETWORK_SHUTDOWN_WRITE 2u
#define AMATERASU_NETWORK_SHUTDOWN_BOTH 3u
#define AMATERASU_NETWORK_LISTEN_NONBLOCKING 1u
#define AMATERASU_NETWORK_LISTEN_REUSE_ADDRESS 2u
#define AMATERASU_NETWORK_LISTEN_IPV6_ONLY 4u

/* family is 4 or 6. Port, scope_id and flowinfo are native integer values.
 * IPv4 occupies bytes[0..4], with remaining bytes, scope and flow zero.
 * IPv6 occupies all sixteen bytes. Reserved values must be zero. */
typedef struct {
    uint32_t family;
    uint16_t port;
    uint16_t reserved;
    uint32_t scope_id;
    uint32_t flowinfo;
    uint8_t bytes[16];
} AmaterasuNetworkAddress;

typedef struct {
    AmaterasuNetworkAddress address;
    uint32_t interface_index;
    uint32_t reserved;
} AmaterasuNetworkInterfaceAddress;

typedef struct {
    uint32_t size;
    uint32_t version;
    uint64_t features;
    uint64_t max_io_bytes;
    uint32_t max_handles;
    uint32_t max_batch;
} AmaterasuNetworkCapabilities;

typedef struct {
    uint32_t size;
    uint32_t flags;
    uint32_t backlog;
    uint32_t reserved;
} AmaterasuNetworkListenOptions;

/* Data remains immutable and live only for the synchronous send_batch call. */
typedef struct {
    const uint8_t *data;
    size_t len;
} AmaterasuNetworkDatagram;

/* All callbacks are thread-safe, use principal-bound opaque handles, and never
 * unwind. Providers own every handle and release its backing resources.
 * All byte buffers and descriptor arrays are synchronous borrows, not ownership
 * transfers. Callers retain provider code through every call and child handle.
 * release(context) follows all concurrent calls and closes binding resources.
 *
 * TCP read/write can return a partial count; EOF is success with zero bytes.
 * UDP operations act on one datagram at a time, including zero-byte datagrams;
 * consumers must supply adequate receive capacity for their protocol.
 * WOULD_BLOCK accepts no bytes. send_batch reports the accepted datagram prefix:
 * success plus nonzero count takes precedence over a later send error. Retry
 * only the remaining tail. Counts can never exceed supplied buffer/array bounds.
 * Capability bits and limits must be checked before optional operations.
 * Blocking/timeout operations belong on consumer workers, never a host tick.
 * This is a native ABI, not a network wire format or a native-code sandbox. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*capabilities)(void *, AmaterasuNetworkCapabilities *);
    AmaterasuStatus (*resolve)(void *, AmaterasuBorrowedBytes, uint16_t, uint32_t,
        AmaterasuNetworkAddress *, size_t, size_t *);
    AmaterasuStatus (*interfaces)(void *, AmaterasuNetworkInterfaceAddress *, size_t, size_t *);
    AmaterasuStatus (*tcp_listen)(void *, AmaterasuNetworkAddress,
        AmaterasuNetworkListenOptions, uint64_t *);
    AmaterasuStatus (*tcp_connect)(void *, AmaterasuNetworkAddress, uint32_t, uint64_t *);
    AmaterasuStatus (*udp_bind)(void *, AmaterasuNetworkAddress, uint64_t *);
    AmaterasuStatus (*accept)(void *, uint64_t, uint64_t *, AmaterasuNetworkAddress *);
    AmaterasuStatus (*local_addr)(void *, uint64_t, AmaterasuNetworkAddress *);
    AmaterasuStatus (*peer_addr)(void *, uint64_t, AmaterasuNetworkAddress *);
    AmaterasuStatus (*read)(void *, uint64_t, uint8_t *, size_t, size_t *);
    AmaterasuStatus (*peek)(void *, uint64_t, uint8_t *, size_t, size_t *);
    AmaterasuStatus (*write)(void *, uint64_t, const uint8_t *, size_t, size_t *);
    AmaterasuStatus (*recv_from)(void *, uint64_t, uint8_t *, size_t, size_t *,
        AmaterasuNetworkAddress *);
    AmaterasuStatus (*send_to)(void *, uint64_t, const uint8_t *, size_t,
        AmaterasuNetworkAddress, size_t *);
    AmaterasuStatus (*connect_udp)(void *, uint64_t, AmaterasuNetworkAddress);
    AmaterasuStatus (*set_option)(void *, uint64_t, uint32_t, uint64_t);
    AmaterasuStatus (*get_option)(void *, uint64_t, uint32_t, uint64_t *);
    AmaterasuStatus (*join_multicast)(void *, uint64_t, AmaterasuNetworkAddress, uint32_t, bool);
    AmaterasuStatus (*send_batch)(void *, uint64_t, const AmaterasuNetworkDatagram *, size_t, size_t *);
    AmaterasuStatus (*shutdown)(void *, uint64_t, uint32_t);
    AmaterasuStatus (*close)(void *, uint64_t);
} AmaterasuNetworkApi;

#define AMATERASU_NETWORK_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuNetworkApi))

/* Optional v1.1 diagnostics tail. Bind the same immutable contract and retain the
 * v1 prefix; use only when size, minor and capability all advertise the tail.
 * Unknown timing has flags=0 and must never be represented as measured zero. */
#define AMATERASU_NETWORK_DIAGNOSTICS_MINOR 1u
#define AMATERASU_NETWORK_CAP_DATAGRAM_DIAGNOSTICS UINT64_C(64)
#define AMATERASU_NETWORK_KERNEL_RESIDENCE_REALTIME UINT64_C(1)
#define AMATERASU_NETWORK_SOCKET_MEMORY UINT64_C(2)
typedef struct {
    uint32_t size;
    uint32_t version;
    uint64_t flags;
    uint64_t kernel_residence_us;
} AmaterasuNetworkReceiveTiming;
typedef struct {
    uint32_t size;
    uint32_t version;
    uint64_t flags;
    uint64_t receive_memory_bytes;
    uint64_t receive_buffer_bytes;
    uint64_t send_memory_bytes;
    uint64_t send_buffer_bytes;
    uint64_t receive_drops;
} AmaterasuNetworkSocketDiagnostics;
typedef struct {
    AmaterasuNetworkApi v1;
    AmaterasuStatus (*recv_from_timed)(void *, uint64_t, uint8_t *, size_t,
        size_t *, AmaterasuNetworkAddress *, AmaterasuNetworkReceiveTiming *);
    AmaterasuStatus (*socket_diagnostics)(void *, uint64_t, AmaterasuNetworkSocketDiagnostics *);
} AmaterasuNetworkApiV1_1;

#ifdef __cplusplus
}
#endif
#endif
