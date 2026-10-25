// Private ABI: Rust retains both NUL-terminated input strings and the writable
// result until return. The linked library owns its socket and request deadline.
// No C++ object or exception crosses this boundary.
#include <virtual_display/driver/linux_control_client.h>
#include <cerrno>
#include <cstdint>
#include <cstring>
#include <exception>

struct AmaterasuLinuxDisplayResult {
    uint32_t status;
    int32_t connected;
    int32_t native_error;
    char detail[192];
};

extern "C" int amaterasu_linux_display_control(const char *socket_path,
    const char *output, uint32_t action, AmaterasuLinuxDisplayResult *result) noexcept {
    if (!socket_path || !output || !result || action > 2) return -1;
    *result = {};
    result->connected = -1;
    try {
        const virtual_display::driver::LinuxControlClient client{socket_path};
        const auto reply = action == 0 ? client.query_connector(output)
            : client.set_connector(output, action == 1);
        result->status = static_cast<uint32_t>(reply.status);
        if (reply.connected.has_value()) result->connected = *reply.connected ? 1 : 0;
        result->native_error = reply.native_error;
        const auto length = reply.detail.size() < sizeof(result->detail) - 1
            ? reply.detail.size() : sizeof(result->detail) - 1;
        std::memcpy(result->detail, reply.detail.data(), length);
        return 0;
    } catch (const std::exception &) {
        result->status = static_cast<uint32_t>(virtual_display::driver::LinuxControlStatus::TransportFailed);
        result->native_error = EIO;
        return 0;
    } catch (...) {
        result->status = static_cast<uint32_t>(virtual_display::driver::LinuxControlStatus::TransportFailed);
        result->native_error = EIO;
        return 0;
    }
}

extern "C" {
__attribute__((used)) extern const char amaterasu_linux_vdd_marker[] = "AMATERASU_LINUX_VDD=b2946d02a09bc453d9e6d287623390ca1dedf964";
}
