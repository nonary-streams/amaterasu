#ifndef AMATERASU_LOGGING_H
#define AMATERASU_LOGGING_H

#include "amaterasu.h"
#include <string.h>

/* Logging uses the existing host endpoint and authenticated module principal.
 * It is a control-plane call: do not invoke it from a real-time callback.
 * Input text must be UTF-8; exclude credentials and private request data.
 * A successful call means the host accepted the record, not that it was saved.
 * The caller provides scratch space; 46064 bytes accommodates worst-case JSON.
 */
#define AMATERASU_LOG_SCRATCH_SIZE 46064u
typedef enum {
    AMATERASU_LOG_VERBOSE = 0, AMATERASU_LOG_DEBUG = 1,
    AMATERASU_LOG_INFO = 2, AMATERASU_LOG_WARNING = 3,
    AMATERASU_LOG_ERROR = 4, AMATERASU_LOG_FATAL = 5
} AmaterasuLogLevel;

typedef struct { uint8_t *data; size_t length; size_t capacity; } AmaterasuLogBuffer;

static inline int amaterasu_log_append(AmaterasuLogBuffer *out, const void *data, size_t length) {
    if (length > out->capacity - out->length) return 0;
    memcpy(out->data + out->length, data, length);
    out->length += length;
    return 1;
}

static inline int amaterasu_log_json_string(AmaterasuLogBuffer *out, const uint8_t *data, size_t length) {
    static const char hex[] = "0123456789abcdef";
    if (!amaterasu_log_append(out, "\"", 1)) return 0;
    for (size_t i = 0; i < length; ++i) {
        uint8_t c = data[i];
        if (c == '"' || c == '\\') {
            char escaped[2] = {'\\', (char)c};
            if (!amaterasu_log_append(out, escaped, sizeof(escaped))) return 0;
        } else if (c < 0x20) {
            char escaped[6] = {'\\', 'u', '0', '0', hex[c >> 4], hex[c & 15]};
            if (!amaterasu_log_append(out, escaped, sizeof(escaped))) return 0;
        } else if (!amaterasu_log_append(out, &c, 1)) return 0;
    }
    return amaterasu_log_append(out, "\"", 1);
}

typedef struct { int accepted; unsigned calls; } AmaterasuLogReply;
static inline void amaterasu_log_reply(void *context, AmaterasuBorrowedBytes bytes) {
    AmaterasuLogReply *reply = (AmaterasuLogReply *)context;
    static const char ok[] = "{\"kind\":\"ok\"}";
    reply->calls++;
    reply->accepted = bytes.ptr != NULL && bytes.len == sizeof(ok) - 1 &&
                      memcmp(bytes.ptr, ok, sizeof(ok) - 1) == 0;
}

static inline AmaterasuStatus amaterasu_log_emit(
    const AmaterasuHostApi *host, AmaterasuLogLevel level,
    const char *target, size_t target_len, const char *message, size_t message_len,
    uint8_t *scratch, size_t scratch_len) {
    if (!host || host->header.major != AMATERASU_ABI_MAJOR ||
        host->header.size < sizeof(*host) || !host->invoke ||
        level < AMATERASU_LOG_VERBOSE || level > AMATERASU_LOG_FATAL ||
        !target || !target_len || target_len > 128 || !message || message_len > 7500 || !scratch)
        return AMATERASU_STATUS_INVALID_ARGUMENT;
    for (size_t i = 0; i < target_len; ++i)
        if ((unsigned char)target[i] < 0x20 || (unsigned char)target[i] == 0x7f)
            return AMATERASU_STATUS_INVALID_ARGUMENT;
    if (memchr(message, 0, message_len)) return AMATERASU_STATUS_INVALID_ARGUMENT;
    AmaterasuLogBuffer out = {scratch, 0, scratch_len};
    const char prefix[] = "{\"op\":\"log\",\"level\":";
    const char target_key[] = ",\"target\":";
    const char message_key[] = ",\"message\":";
    char severity = (char)('0' + level);
    if (!amaterasu_log_append(&out, prefix, sizeof(prefix) - 1) ||
        !amaterasu_log_append(&out, &severity, 1) ||
        !amaterasu_log_append(&out, target_key, sizeof(target_key) - 1) ||
        !amaterasu_log_json_string(&out, (const uint8_t *)target, target_len) ||
        !amaterasu_log_append(&out, message_key, sizeof(message_key) - 1) ||
        !amaterasu_log_json_string(&out, (const uint8_t *)message, message_len) ||
        !amaterasu_log_append(&out, "}", 1)) return AMATERASU_STATUS_INVALID_ARGUMENT;
    AmaterasuLogReply reply = {0, 0};
    AmaterasuReplySink sink = {&reply, amaterasu_log_reply};
    AmaterasuBorrowedBytes request = {out.data, out.length};
    AmaterasuStatus status = host->invoke(host->context, request, sink);
    return status == AMATERASU_STATUS_OK && reply.calls == 1 && reply.accepted
        ? AMATERASU_STATUS_OK : AMATERASU_STATUS_INTERNAL_ERROR;
}
#endif
