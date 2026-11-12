#include "amaterasu_data_plane.h"

struct reversed { uint8_t data[4]; size_t size; };
static AmaterasuStatus reverse_input(void *ctx, const AmaterasuDataChunk *input) {
    struct reversed *out = ctx;
    if (input->metadata_len != 0 || input->segment_count != 1 || input->segments[0].len > sizeof(out->data)) return AMATERASU_STATUS_INVALID_ARGUMENT;
    out->size = input->segments[0].len;
    for (size_t i = 0; i < out->size; ++i) out->data[i] = input->segments[0].data[out->size - i - 1];
    return AMATERASU_STATUS_OK;
}
AmaterasuStatus neutral_exchange(const AmaterasuTransferApi *api, AmaterasuDataChannelId source, AmaterasuDataChannelId sink) {
    AmaterasuDataDescription in, out;
    if (api->describe(api->base.context, source, &in) != AMATERASU_STATUS_OK || api->describe(api->base.context, sink, &out) != AMATERASU_STATUS_OK) return AMATERASU_STATUS_INVALID_ARGUMENT;
    if (in.direction != AMATERASU_DATA_SOURCE || out.direction != AMATERASU_DATA_SINK || in.limits.max_metadata_bytes || out.limits.max_metadata_bytes) return AMATERASU_STATUS_INVALID_ARGUMENT;
    struct reversed bytes = {{0}, 0};
    AmaterasuDataChunkSink callback = {&bytes, reverse_input};
    AmaterasuStatus status = api->receive(api->base.context, source, callback);
    if (status != AMATERASU_STATUS_OK) return status;
    if (bytes.size == 0) return AMATERASU_STATUS_INVALID_ARGUMENT;
    AmaterasuDataSegment part = {bytes.data, bytes.size};
    AmaterasuDataChunk record = {sizeof(record), 0, 1, 0, bytes.size, &part, 1, 0, NULL, NULL};
    return api->submit(api->base.context, sink, &record);
}
