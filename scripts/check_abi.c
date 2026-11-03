#include "amaterasu.h"

/* Layout checks for the supported 64-bit host targets. */
_Static_assert(sizeof(void *) == 8, "Amaterasu currently targets 64-bit platforms");
_Static_assert(sizeof(AmaterasuAbiHeader) == 12, "header layout changed");
_Static_assert(sizeof(AmaterasuBorrowedBytes) == 16, "borrowed bytes layout changed");
_Static_assert(sizeof(AmaterasuReplySink) == 16, "reply layout changed");
_Static_assert(sizeof(AmaterasuHostApi) == 32, "host table layout changed");
_Static_assert(sizeof(AmaterasuModuleApi) == 48, "module table layout changed");
_Static_assert(sizeof(AmaterasuModuleQuiesceApi) == 24, "quiesce table layout changed");
_Static_assert(sizeof(AmaterasuRuntimeApi) == 40, "runtime table layout changed");
_Static_assert(sizeof(AmaterasuRuntimeWaitApi) == 32, "runtime wait table layout changed");
_Static_assert(offsetof(AmaterasuModuleApi, create) == 16, "function alignment changed");

int main(void) {
    return 0;
}
