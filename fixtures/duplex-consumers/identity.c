#include "amaterasu_data_plane.h"
#include <string.h>

struct payload { uint8_t bytes[4]; size_t len; };
static AmaterasuStatus capture(void *ctx, const AmaterasuDataChunk *chunk) {
    struct payload *p = ctx;
    if (chunk->metadata_len || chunk->segment_count != 1 || chunk->segments[0].len > sizeof(p->bytes)) return AMATERASU_STATUS_INVALID_ARGUMENT;
    p->len = chunk->segments[0].len;
    memcpy(p->bytes, chunk->segments[0].data, p->len);
    return AMATERASU_STATUS_OK;
}
AmaterasuStatus neutral_exchange(const AmaterasuTransferApi *api, AmaterasuDataChannelId source, AmaterasuDataChannelId sink) {
    AmaterasuDataDescription a, b;
    if (api->describe(api->base.context, source, &a) || api->describe(api->base.context, sink, &b) || a.direction != AMATERASU_DATA_SOURCE || b.direction != AMATERASU_DATA_SINK || a.limits.max_metadata_bytes || b.limits.max_metadata_bytes) return AMATERASU_STATUS_INVALID_ARGUMENT;
    struct payload p = {{0}, 0};
    AmaterasuDataChunkSink receiver = {&p, capture};
    AmaterasuStatus result = api->receive(api->base.context, source, receiver);
    if (result) return result;
    if (!p.len) return AMATERASU_STATUS_INVALID_ARGUMENT;
    AmaterasuDataSegment segment = {p.bytes, p.len};
    AmaterasuDataChunk chunk = {sizeof(chunk), 0, 1, 0, p.len, &segment, 1, 0, NULL, NULL};
    return api->submit(api->base.context, sink, &chunk);
}
