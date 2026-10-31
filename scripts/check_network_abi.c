#include "amaterasu_network.h"

#ifdef __cplusplus
#define CHECK static_assert
#else
#define CHECK _Static_assert
#endif

/* Match crates/network-api's Rust layout on supported 64-bit host targets. */
CHECK(sizeof(void *) == 8, "supported host pointer size");
CHECK(sizeof(bool) == 1, "Rust bool callback argument layout");
CHECK(sizeof(AmaterasuNetworkAddress) == 32, "address");
CHECK(offsetof(AmaterasuNetworkAddress, port) == 4, "port offset");
CHECK(offsetof(AmaterasuNetworkAddress, scope_id) == 8, "scope offset");
CHECK(offsetof(AmaterasuNetworkAddress, bytes) == 16, "address bytes offset");
CHECK(sizeof(AmaterasuNetworkInterfaceAddress) == 40, "interface address");
CHECK(sizeof(AmaterasuNetworkCapabilities) == 32, "capabilities");
CHECK(offsetof(AmaterasuNetworkCapabilities, features) == 8, "features offset");
CHECK(offsetof(AmaterasuNetworkCapabilities, max_io_bytes) == 16, "I/O limit offset");
CHECK(sizeof(AmaterasuNetworkListenOptions) == 16, "listen options");
CHECK(sizeof(AmaterasuNetworkDatagram) == 16, "datagram descriptor");
CHECK(sizeof(AmaterasuNetworkApi) == 200, "network table");
CHECK(offsetof(AmaterasuNetworkApi, capabilities) == 32, "capabilities callback offset");
CHECK(offsetof(AmaterasuNetworkApi, tcp_listen) == 56, "listen callback offset");
CHECK(offsetof(AmaterasuNetworkApi, read) == 104, "read callback offset");
CHECK(offsetof(AmaterasuNetworkApi, send_batch) == 176, "batch callback offset");
CHECK(offsetof(AmaterasuNetworkApi, close) == 192, "close callback offset");

int main(void) { return 0; }
