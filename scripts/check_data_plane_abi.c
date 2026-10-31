#include "amaterasu_data_plane.h"
#ifdef __cplusplus
#define CHECK static_assert
#else
#define CHECK _Static_assert
#endif
CHECK(sizeof(void *) == 8, "supported host pointer size");
CHECK(sizeof(AmaterasuDataSchema) == 24, "schema");
CHECK(sizeof(AmaterasuDataChannelId) == 16, "channel");
CHECK(sizeof(AmaterasuDataLimits) == 40, "limits");
CHECK(sizeof(AmaterasuDataDescription) == 104, "description");
CHECK(sizeof(AmaterasuDataSegment) == 16, "segment");
CHECK(sizeof(AmaterasuDataBufferLease) == 32, "lease");
CHECK(sizeof(AmaterasuDataChunk) == 64, "chunk");
CHECK(sizeof(AmaterasuDataSnapshot) == 24, "snapshot");
CHECK(sizeof(AmaterasuTransferApi) == 88, "table");
CHECK(offsetof(AmaterasuTransferApi, describe) == 32, "describe offset");
CHECK(offsetof(AmaterasuTransferApi, cancel) == 80, "cancel offset");
CHECK(offsetof(AmaterasuDataChunk, segments) == 32, "segments offset");
CHECK(offsetof(AmaterasuDataChunk, lease) == 56, "lease offset");
int main(void) { return 0; }
