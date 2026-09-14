#include "amaterasu_identity.h"
#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(AmaterasuIdentityApi) == 56, "identity table layout");
_Static_assert(offsetof(AmaterasuIdentityApi, certificate) == 32, "certificate offset");
_Static_assert(offsetof(AmaterasuIdentityApi, sign) == 40, "sign offset");
_Static_assert(offsetof(AmaterasuIdentityApi, verify_client) == 48, "client trust offset");
#endif
