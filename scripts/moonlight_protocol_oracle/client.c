/* Test-only callbacks around the actual Moonlight protocol library. No devices. */
#define _POSIX_C_SOURCE 200809L
#include <Limelight.h>
#include <libavcodec/avcodec.h>
#include <opus_multistream.h>
#include <stdatomic.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static atomic_int terminated, decode_errors;
static atomic_ulong video_frames, audio_packets, nonzero_samples;
static AVCodecContext *video_decoder;
static OpusMSDecoder *audio_decoder;
static int audio_channels, audio_frame_samples;

static void log_message(const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    vfprintf(stderr, format, arguments);
    va_end(arguments);
}

static void connection_terminated(int error) {
    fprintf(stderr, "CONNECTION_TERMINATED %d\n", error);
    atomic_store(&terminated, 1);
}
static void stage_failed(int stage, int error) {
    fprintf(stderr, "STAGE_FAILED %s %d\n", LiGetStageName(stage), error);
    atomic_store(&terminated, 1);
}
static int setup_video(int format, int width, int height, int rate, void *context, int flags) {
    (void)context; (void)flags; (void)rate;
    if (format != VIDEO_FORMAT_H264 || width != 16 || height != 16) return -1;
    video_decoder = avcodec_alloc_context3(avcodec_find_decoder(AV_CODEC_ID_H264));
    if (!video_decoder) return -1;
    video_decoder->thread_count = 1;
    return avcodec_open2(video_decoder, video_decoder->codec, NULL);
}
static int decode_video(PDECODE_UNIT unit) {
    AVPacket *packet = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    if (!packet || !frame || unit->fullLength <= 0 || unit->fullLength > 1024 * 1024 ||
        av_new_packet(packet, unit->fullLength) < 0) goto failure;
    int copied = 0;
    for (PLENTRY entry = unit->bufferList; entry; entry = entry->next) {
        if (entry->length <= 0 || entry->length > unit->fullLength - copied) goto failure;
        memcpy(packet->data + copied, entry->data, entry->length);
        copied += entry->length;
    }
    if (copied != unit->fullLength || avcodec_send_packet(video_decoder, packet) < 0) goto failure;
    int result;
    while ((result = avcodec_receive_frame(video_decoder, frame)) == 0) {
        if (frame->width != 16 || frame->height != 16) goto failure;
        atomic_fetch_add(&video_frames, 1);
        av_frame_unref(frame);
    }
    if (result != AVERROR(EAGAIN) && result != AVERROR_EOF) goto failure;
    av_packet_free(&packet); av_frame_free(&frame);
    return DR_OK;
failure:
    atomic_fetch_add(&decode_errors, 1);
    av_packet_free(&packet); av_frame_free(&frame);
    return DR_NEED_IDR;
}
static void cleanup_video(void) { avcodec_free_context(&video_decoder); }
static int setup_audio(int configuration, const POPUS_MULTISTREAM_CONFIGURATION opus,
                       void *context, int flags) {
    (void)configuration; (void)context; (void)flags;
    if (opus->sampleRate != 48000 || opus->channelCount != 2 ||
        opus->samplesPerFrame <= 0 || opus->samplesPerFrame > 2880) return -1;
    int error;
    audio_channels = opus->channelCount;
    audio_frame_samples = opus->samplesPerFrame;
    audio_decoder = opus_multistream_decoder_create(opus->sampleRate, opus->channelCount,
        opus->streams, opus->coupledStreams, opus->mapping, &error);
    return error;
}
static void decode_audio(char *data, int length) {
    short pcm[5760];
    int samples = opus_multistream_decode(audio_decoder, (unsigned char *)data, length,
                                          pcm, audio_frame_samples, 0);
    if (samples != audio_frame_samples) {
        atomic_fetch_add(&decode_errors, 1);
        return;
    }
    atomic_fetch_add(&audio_packets, 1);
    for (int i = 0; i < samples * audio_channels; ++i)
        if (pcm[i]) atomic_fetch_add(&nonzero_samples, 1);
}
static void cleanup_audio(void) { opus_multistream_decoder_destroy(audio_decoder); audio_decoder = NULL; }
static unsigned long milliseconds(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (unsigned long)now.tv_sec * 1000 + (unsigned long)now.tv_nsec / 1000000;
}
int main(int argc, char **argv) {
    if (argc != 4 || strncmp(argv[1], "rtspenc://127.0.0.1:", 20) != 0) return 2;
    char *end;
    unsigned long duration = strtoul(argv[3], &end, 10);
    if (*end || duration < 1000 || duration > 180000) return 2;
    SERVER_INFORMATION server;
    STREAM_CONFIGURATION config;
    CONNECTION_LISTENER_CALLBACKS listener;
    DECODER_RENDERER_CALLBACKS video;
    AUDIO_RENDERER_CALLBACKS audio;
    LiInitializeServerInformation(&server); LiInitializeStreamConfiguration(&config);
    LiInitializeConnectionCallbacks(&listener); LiInitializeVideoCallbacks(&video);
    LiInitializeAudioCallbacks(&audio);
    server.address = "127.0.0.1";
    server.serverInfoAppVersion = "7.1.431.-1";
    server.serverInfoGfeVersion = "3.23.0.74";
    server.serverCodecModeSupport = SCM_H264;
    server.rtspSessionUrl = argv[1];
    config.width = 16; config.height = 16; config.fps = 30;
    config.bitrate = 20000; config.packetSize = 1024;
    config.streamingRemotely = STREAM_CFG_LOCAL;
    config.audioConfiguration = AUDIO_CONFIGURATION_STEREO;
    config.supportedVideoFormats = VIDEO_FORMAT_H264;
    config.encryptionFlags = ENCFLG_ALL;
    config.remoteInputAesIv[3] = 17;
    FILE *key = fopen(argv[2], "rb");
    if (!key) return 2;
    int valid_key = fread(config.remoteInputAesKey, 1, 16, key) == 16 && fgetc(key) == EOF;
    fclose(key);
    if (!valid_key) return 2;
    listener.stageFailed = stage_failed;
    listener.logMessage = log_message;
    listener.connectionTerminated = connection_terminated;
    video.setup = setup_video; video.submitDecodeUnit = decode_video; video.cleanup = cleanup_video;
    video.capabilities = CAPABILITY_DIRECT_SUBMIT;
    audio.init = setup_audio; audio.decodeAndPlaySample = decode_audio; audio.cleanup = cleanup_audio;
    audio.capabilities = CAPABILITY_DIRECT_SUBMIT;
    int result = LiStartConnection(&server, &config, &listener, &video, &audio, NULL, 0, NULL, 0);
    if (result) { fprintf(stderr, "START_FAILED %d\n", result); return 1; }
    unsigned long start = milliseconds(), last_video = 0, last_audio = 0;
    int stalled = 0;
    while (milliseconds() - start < duration && !atomic_load(&terminated)) {
        struct timespec delay = {.tv_sec = 1, .tv_nsec = 0};
        nanosleep(&delay, NULL);
        unsigned long frames = atomic_load(&video_frames), packets = atomic_load(&audio_packets);
        printf("SAMPLE %lu %lu %lu\n", milliseconds() - start, frames, packets); fflush(stdout);
        if (milliseconds() - start > 3000 && (frames == last_video || packets == last_audio)) stalled = 1;
        last_video = frames; last_audio = packets;
        if (stalled || atomic_load(&decode_errors)) break;
    }
    unsigned long elapsed = milliseconds() - start;
    LiStopConnection();
    printf("RESULT elapsed_ms=%lu video=%lu audio=%lu nonzero=%lu errors=%d terminated=%d stalled=%d\n",
        elapsed, atomic_load(&video_frames), atomic_load(&audio_packets), atomic_load(&nonzero_samples),
        atomic_load(&decode_errors), atomic_load(&terminated), stalled);
    return elapsed >= duration && !atomic_load(&terminated) && !stalled && !atomic_load(&decode_errors)
        && atomic_load(&video_frames) >= duration / 100 && atomic_load(&audio_packets) >= duration / 20
        && atomic_load(&nonzero_samples) > 0 ? 0 : 1;
}
