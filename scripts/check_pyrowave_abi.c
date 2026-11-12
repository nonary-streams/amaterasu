/* Verify the independently declared Rust boundary against pinned upstream 0.6. */
#include <stddef.h>
#include <stdint.h>
#include <vulkan/vulkan_core.h>
#include <pyrowave.h>
_Static_assert(PYROWAVE_API_VERSION_MAJOR == 0, "PyroWave ABI major changed");
_Static_assert(PYROWAVE_API_VERSION_MINOR == 6, "PyroWave ABI minor changed");
_Static_assert(PYROWAVE_API_VERSION_PATCH == 0, "PyroWave ABI patch changed");
_Static_assert(sizeof(pyrowave_packet) == 2 * sizeof(size_t), "packet layout changed");
_Static_assert(offsetof(pyrowave_encoder_create_info, width) == sizeof(void *), "encoder layout changed");
_Static_assert(offsetof(pyrowave_cpu_buffer, width) == 3 * sizeof(void *) + 6 * sizeof(size_t), "CPU layout changed");
_Static_assert(offsetof(pyrowave_cpu_buffer, format) == 3 * sizeof(void *) + 6 * sizeof(size_t) + 2 * sizeof(int), "CPU layout changed");
#if UINTPTR_MAX == UINT64_MAX
_Static_assert(sizeof(pyrowave_cpu_buffer) == 88, "CPU structure size changed");
_Static_assert(sizeof(pyrowave_encoder_create_info) == 24, "encoder structure size changed");
_Static_assert(sizeof(pyrowave_decoder_create_info) == 24, "decoder structure size changed");
#endif
int main(void) { return 0; }
