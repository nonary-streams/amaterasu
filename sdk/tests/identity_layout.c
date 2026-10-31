#include "amaterasu_identity.h"
#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(AmaterasuIdentityApi) == 56, "identity table layout");
_Static_assert(offsetof(AmaterasuIdentityApi, certificate) == 32, "certificate offset");
_Static_assert(offsetof(AmaterasuIdentityApi, sign) == 40, "sign offset");
_Static_assert(offsetof(AmaterasuIdentityApi, verify_client) == 48, "client trust offset");
_Static_assert(sizeof(AmaterasuIdentityApiV1_1) == 64, "identity 1.1 table layout");
_Static_assert(offsetof(AmaterasuIdentityApiV1_1, v1_0) == 0, "identity 1.0 prefix");
_Static_assert(offsetof(AmaterasuIdentityApiV1_1, check_permission) == 56, "permission offset");
#endif
