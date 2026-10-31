#include "amaterasu_encoder_input_pool.h"
#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(AmaterasuEncoderInputCpuWritableCapabilities) == 28, "writable CPU capabilities layout");
_Static_assert(sizeof(AmaterasuEncoderInputCpuWritableFrame) == 32, "writable CPU frame layout");
_Static_assert(offsetof(AmaterasuEncoderInputCpuWritableFrame, data) == 16, "writable CPU data offset");
_Static_assert(sizeof(AmaterasuEncoderInputPoolConfig) == 40, "pool config layout");
_Static_assert(sizeof(AmaterasuEncoderInputTarget) == 104, "pool target layout");
_Static_assert(sizeof(AmaterasuEncoderInputSubmission) == 152, "pool submission layout");
_Static_assert(offsetof(AmaterasuEncoderInputSubmission, completion_sync) == 112, "pool completion offset");
_Static_assert(sizeof(AmaterasuEncoderInputPoolRetirement) == 24, "pool retirement layout");
_Static_assert(sizeof(AmaterasuEncoderInputPoolApiV1) == 88, "pool table layout");
_Static_assert(offsetof(AmaterasuEncoderInputPoolApiV1, enumerate) == 32, "pool prefix layout");
_Static_assert(offsetof(AmaterasuEncoderInputPoolApiV1, begin_retirement) == 72, "pool retirement offset");
_Static_assert(AMATERASU_ENCODER_INPUT_POOL_API_V1_SIZE == sizeof(AmaterasuEncoderInputPoolApiV1), "pool table size");
#endif
