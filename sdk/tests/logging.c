#include "amaterasu_logging.h"
#include <assert.h>
#include <string.h>

static AmaterasuStatus invoke(void *context, AmaterasuBorrowedBytes input, AmaterasuReplySink reply) {
    const char expected[] = "{\"op\":\"log\",\"level\":2,\"target\":\"video\",\"message\":\"quote \\\" and newline \\u000a\"}";
    assert(input.len == sizeof(expected) - 1 && memcmp(input.ptr, expected, input.len) == 0);
    if (context) {
        const char rejected[] = "{\"kind\":\"error\",\"code\":\"invalid_log_record\",\"message\":\"invalid\"}";
        AmaterasuBorrowedBytes bytes = {(const uint8_t *)rejected, sizeof(rejected) - 1};
        reply.write(reply.context, bytes);
        return AMATERASU_STATUS_OK;
    }
    const char ok[] = "{\"kind\":\"ok\"}";
    AmaterasuBorrowedBytes bytes = {(const uint8_t *)ok, sizeof(ok) - 1};
    reply.write(reply.context, bytes);
    return AMATERASU_STATUS_OK;
}
int main(void) {
    AmaterasuHostApi host = {{sizeof(host), 1, 0}, NULL, invoke};
    uint8_t scratch[AMATERASU_LOG_SCRATCH_SIZE];
    const char message[] = "quote \" and newline \n";
    assert(amaterasu_log_emit(&host, AMATERASU_LOG_INFO, "video", 5, message, sizeof(message) - 1, scratch, sizeof(scratch)) == AMATERASU_STATUS_OK);
    host.context = &host;
    assert(amaterasu_log_emit(&host, AMATERASU_LOG_INFO, "video", 5, message, sizeof(message) - 1, scratch, sizeof(scratch)) == AMATERASU_STATUS_INTERNAL_ERROR);
    assert(amaterasu_log_emit(&host, AMATERASU_LOG_INFO, "video", 5, message, sizeof(message) - 1, scratch, 4) == AMATERASU_STATUS_INVALID_ARGUMENT);
    assert(amaterasu_log_emit(&host, AMATERASU_LOG_INFO, "bad\n", 4, message, sizeof(message) - 1, scratch, sizeof(scratch)) == AMATERASU_STATUS_INVALID_ARGUMENT);
    return 0;
}
