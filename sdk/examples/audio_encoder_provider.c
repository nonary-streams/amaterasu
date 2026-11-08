/* Encoder table skeleton. Replace the hooks with an owned, bounded codec queue
 * and register the table's factory with AMATERASU_AUDIO_ENCODER_CONTRACT. */
#include "amaterasu_media.h"

static AmaterasuStatus submit(void *context, uint64_t stream, const AmaterasuAudioFrame *frame) {
    (void)context; (void)stream; (void)frame;
    /* Copy input if retained. BUSY must leave it unaccepted. */
    return AMATERASU_MEDIA_UNSUPPORTED;
}

static AmaterasuStatus receive(void *context, uint64_t stream, AmaterasuAudioUnitSink sink) {
    (void)context; (void)stream; (void)sink;
    /* Offer one queued unit with sink.write; retain it unless accepted. */
    return AMATERASU_MEDIA_PENDING;
}

static AmaterasuStatus drain(void *context, uint64_t stream) {
    (void)context; (void)stream;
    /* Stop accepting input, flush the queue, then deliver an END unit. */
    return AMATERASU_MEDIA_UNSUPPORTED;
}

AmaterasuAudioEncoderApi example_audio_encoder(void *context) {
    AmaterasuAudioEncoderApi api = {
        .base = { .header = { sizeof(AmaterasuAudioEncoderApi), 1, 0 },
                  .context = context, .release = NULL },
        .submit = submit, .receive = receive, .drain = drain,
    };
    return api;
}
