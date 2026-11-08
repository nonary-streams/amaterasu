/* Isolated physical KMS readback worker. Its only input is one inherited Unix
 * stream at fd 3. The launcher executes a verified sealed image, supplies an
 * empty environment and retains process ownership until waitpid confirms exit.
 * Records are fixed little-endian fields; pixel data is bounded to 128 MiB. */
#define _GNU_SOURCE
#include "adapter.h"
#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>

enum { CONTROL_FD = 3, HOST_PIDFD = 4, REQUEST_SIZE = 200, HEADER_SIZE = 24, OUTPUT_SIZE = 416 };
enum { OPEN = 1, READ = 2, CLOSE = 3, PROBE = 4, ENUMERATE = 5 };
#define MAX_FRAME_BYTES (128U * 1024U * 1024U)

static uint32_t get_u32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void put_u32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)v; p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16); p[3] = (uint8_t)(v >> 24);
}

static bool transfer(void *buffer, size_t size, bool writing) {
    uint8_t *p = buffer;
    while (size) {
        ssize_t count = writing ? write(CONTROL_FD, p, size) : read(CONTROL_FD, p, size);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) return false;
        p += (size_t)count; size -= (size_t)count;
    }
    return true;
}

static bool reply(uint32_t op, uint32_t status, uint32_t length, uint32_t width, uint32_t height) {
    uint8_t header[HEADER_SIZE] = {'A','M','R','1'};
    put_u32(header + 4, op); put_u32(header + 8, status);
    put_u32(header + 12, length); put_u32(header + 16, width); put_u32(header + 20, height);
    return transfer(header, sizeof(header), true);
}

static void encode_output(uint8_t bytes[OUTPUT_SIZE], const struct am_kms_output *output) {
    put_u32(bytes, (uint32_t)output->left); put_u32(bytes + 4, (uint32_t)output->top);
    put_u32(bytes + 8, output->width); put_u32(bytes + 12, output->height);
    put_u32(bytes + 16, output->rotation); put_u32(bytes + 20, output->attached);
    put_u32(bytes + 24, output->framebuffer_export_supported); put_u32(bytes + 28, output->error);
    memcpy(bytes + 32, output->id, sizeof(output->id));
    memcpy(bytes + 224, output->name, sizeof(output->name));
    memcpy(bytes + 352, output->card, sizeof(output->card));
}

static bool valid_padding(const uint8_t *payload, bool open) {
    size_t first_nul = 0;
    while (first_nul < AM_KMS_ID_CAPACITY && payload[first_nul] != 0) first_nul++;
    if (first_nul == AM_KMS_ID_CAPACITY || (open && first_nul == 0) || (!open && first_nul != 0)) return false;
    for (size_t i = first_nul; i < AM_KMS_ID_CAPACITY; i++) if (payload[i] != 0) return false;
    return true;
}

static bool close_extra_descriptors(void) {
#ifdef SYS_close_range
    if (syscall(SYS_close_range, 5U, UINT_MAX, 0U) == 0) return true;
    if (errno != ENOSYS && errno != EINVAL) return false;
#endif
    struct rlimit limit;
    if (getrlimit(RLIMIT_NOFILE, &limit) != 0 || limit.rlim_max == RLIM_INFINITY || limit.rlim_max > INT_MAX) return false;
    for (int fd = 5; fd < (int)limit.rlim_max; fd++) (void)close(fd);
    return true;
}

/* A helper can be stuck in a driver call while its host exits. Its dedicated
 * watcher observes the exact inherited host pidfd, independently of the wire
 * or driver thread. This sends SIGKILL only to the still-running helper itself. */
static void *watch_host(void *unused) {
    (void)unused;
    struct pollfd host = {.fd = HOST_PIDFD, .events = POLLIN};
    for (;;) {
        int result = poll(&host, 1, -1);
        if (result < 0 && errno == EINTR) continue;
        if (result != 0) {
            (void)kill(getpid(), SIGKILL);
            _exit(1);
        }
    }
}

static bool start_host_watcher(void) {
    /* A signal number of zero validates both pidfd identity and permission
     * without signalling the host. ESRCH also fails closed. */
    if (syscall(SYS_pidfd_send_signal, HOST_PIDFD, 0, NULL, 0U) != 0) return false;
    pthread_t watcher;
    if (pthread_create(&watcher, NULL, watch_host, NULL) != 0) return false;
    if (pthread_detach(watcher) != 0) return false;
    return true;
}

int main(void) {
    int kind = 0, domain = 0;
    socklen_t length = sizeof(kind);
    if (getsockopt(CONTROL_FD, SOL_SOCKET, SO_TYPE, &kind, &length) != 0 || kind != SOCK_STREAM) return 1;
    length = sizeof(domain);
    if (getsockopt(CONTROL_FD, SOL_SOCKET, SO_DOMAIN, &domain, &length) != 0 || domain != AF_UNIX) return 1;
    if (!close_extra_descriptors() || prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 ||
        !start_host_watcher() || prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0) != 0) return 1;
    (void)signal(SIGPIPE, SIG_IGN);
    (void)close(0); (void)close(1); (void)close(2);
    uint8_t ready[] = {'A','M','K','R','E','A','D','Y'};
    if (!transfer(ready, sizeof(ready), true)) return 1;
    struct am_kms_session *session = NULL;
    struct am_kms_output geometry = {0};
    uint8_t request[REQUEST_SIZE];
    while (transfer(request, sizeof(request), false)) {
        uint32_t op = get_u32(request + 4);
        if (memcmp(request, "AMK1", 4) != 0 || op < OPEN || op > ENUMERATE ||
            !valid_padding(request + 8, op == OPEN)) break;
        if (op == OPEN) {
            if (session != NULL) break;
            uint32_t status = 0;
            session = am_kms_open((const char *)(request + 8), &status);
            if (session == NULL) {
                if (status == 0) status = AM_KMS_CAPTURE_FAILED;
                if (!reply(op, status, 0, 0, 0)) break;
                continue;
            }
            int result = am_kms_session_geometry(session, &geometry);
            if (result != 0) {
                if (!reply(op, (uint32_t)result, 0, 0, 0)) break;
                break;
            }
            uint8_t output[OUTPUT_SIZE];
            encode_output(output, &geometry);
            if (!reply(op, 0, sizeof(output), 0, 0) || !transfer(output, sizeof(output), true)) break;
        } else if (op == READ) {
            if (session == NULL || geometry.width == 0 || geometry.height == 0 ||
                geometry.width > 8192 || geometry.height > 8192) break;
            uint64_t bytes = (uint64_t)geometry.width * geometry.height * 4U;
            if (bytes > MAX_FRAME_BYTES) break;
            uint8_t *pixels = malloc((size_t)bytes);
            if (pixels == NULL) { if (!reply(op, AM_KMS_CAPTURE_FAILED, 0, 0, 0)) break; continue; }
            uint32_t width = 0, height = 0;
            int status = am_kms_read(session, pixels, (size_t)bytes, &width, &height);
            if (status == 0 && (width != geometry.width || height != geometry.height)) status = AM_KMS_CAPTURE_FAILED;
            bool sent = status == 0
                ? reply(op, 0, (uint32_t)bytes, width, height) && transfer(pixels, (size_t)bytes, true)
                : reply(op, (uint32_t)status, 0, 0, 0);
            free(pixels);
            if (!sent) break;
        } else if (op == CLOSE) {
            if (session == NULL) break;
            am_kms_close(session); session = NULL;
            (void)reply(op, 0, 0, 0, 0);
            return 0;
        } else if (op == PROBE) {
            struct am_kms_info info = {0};
            (void)am_kms_probe(&info);
            uint8_t data[16];
            put_u32(data, info.available); put_u32(data + 4, info.output_count);
            put_u32(data + 8, info.framebuffer_export_supported); put_u32(data + 12, info.error);
            if (!reply(op, 0, sizeof(data), 0, 0) || !transfer(data, sizeof(data), true)) break;
        } else {
            struct am_kms_output outputs[AM_KMS_MAX_OUTPUTS];
            size_t count = 0;
            int status = am_kms_enumerate(outputs, AM_KMS_MAX_OUTPUTS, &count);
            if (status != 0) {
                if (!reply(op, (uint32_t)status, 0, 0, 0)) break;
                continue;
            }
            if (count > AM_KMS_MAX_OUTPUTS) break;
            uint32_t length = 4U + (uint32_t)count * OUTPUT_SIZE;
            uint8_t count_bytes[4]; put_u32(count_bytes, (uint32_t)count);
            if (!reply(op, 0, length, 0, 0) || !transfer(count_bytes, sizeof(count_bytes), true)) break;
            bool sent = true;
            for (size_t i = 0; i < count; ++i) {
                uint8_t output[OUTPUT_SIZE]; encode_output(output, &outputs[i]);
                if (!transfer(output, sizeof(output), true)) { sent = false; break; }
            }
            if (!sent) break;
        }
    }
    if (session != NULL) am_kms_close(session);
    return 0;
}
