#include "amaterasu_encoder.h"
#include <stddef.h>
#include <stdint.h>
_Static_assert(offsetof(AmaterasuEncoderProviderApi, base) == 0, "base prefix");
_Static_assert(offsetof(AmaterasuEncoderProviderApi, probe) == sizeof(AmaterasuNativeInterface), "table prefix");
_Static_assert(sizeof(AmaterasuEncodeTimingSnapshotV1) == 48, "timing snapshot");
_Static_assert(offsetof(AmaterasuEncoderApiV1_3, encode_timing) == sizeof(AmaterasuEncoderApiV1_2), "encoder timing tail");
_Static_assert(offsetof(AmaterasuEncoderProviderApiV1_1, encode_timing) == sizeof(AmaterasuEncoderProviderApi), "provider timing tail");
_Static_assert(offsetof(AmaterasuVideoApiV1_4, encode_timing) == sizeof(AmaterasuVideoApiV1_3), "video timing tail");
_Static_assert(sizeof(AmaterasuEncoderFormatId) == 24, "format ID");
_Static_assert(AMATERASU_ENCODER_PROVIDER_MAX_CONTROL_RECORDS == 1024u, "control records");
_Static_assert(AMATERASU_ENCODER_PROVIDER_MAX_CONTROL_BYTES == 8u * 1024u * 1024u, "control bytes");
#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(AmaterasuEncoderCommonConfigV1) == 136, "common config");
_Static_assert(sizeof(AmaterasuEncoderProviderLimitsV1) == 32, "provider limits");
_Static_assert(AMATERASU_FRAME_RESOURCE_SUBMIT_FORCE_IDR == 1u, "force IDR");
_Static_assert(sizeof(AmaterasuEncoderFrameOptionsV1) == 112, "frame options");
_Static_assert(sizeof(AmaterasuEncoderSessionConfig) == 112, "session config");
_Static_assert(sizeof(AmaterasuEncoderOutputChunk) == 120, "output chunk");
_Static_assert(sizeof(AmaterasuEncoderControlRequest) == 64, "control request");
_Static_assert(sizeof(AmaterasuEncoderControlResponse) == 80, "control response");
_Static_assert(sizeof(AmaterasuEncoderProviderApi) == 96, "provider table");
#endif
int main(void) { return 0; }
