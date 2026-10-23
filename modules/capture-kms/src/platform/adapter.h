#ifndef AMATERASU_KMS_ADAPTER_H
#define AMATERASU_KMS_ADAPTER_H

#include <stddef.h>
#include <stdint.h>

#define AM_KMS_ID_CAPACITY 192
#define AM_KMS_NAME_CAPACITY 128
#define AM_KMS_CARD_CAPACITY 64
#define AM_KMS_MAX_OUTPUTS 64

enum am_kms_error {
    AM_KMS_OK = 0,
    AM_KMS_NO_DEVICE = 1,
    AM_KMS_NO_OUTPUT = 2,
    AM_KMS_ACCESS_DENIED = 3,
    AM_KMS_UNSUPPORTED = 4,
    AM_KMS_INVALID_ARGUMENT = 5,
    AM_KMS_CAPTURE_FAILED = 6,
    AM_KMS_OUTPUT_LOST = 7,
    AM_KMS_TOO_MANY_OUTPUTS = 8,
    AM_KMS_EGL_UNAVAILABLE = 9,
};

struct am_kms_info {
    uint32_t available;
    uint32_t output_count;
    uint32_t framebuffer_export_supported;
    uint32_t error;
};

struct am_kms_output {
    int32_t left;
    int32_t top;
    uint32_t width;
    uint32_t height;
    uint32_t rotation;
    uint32_t attached;
    uint32_t framebuffer_export_supported;
    uint32_t error;
    char id[AM_KMS_ID_CAPACITY];
    char name[AM_KMS_NAME_CAPACITY];
    char card[AM_KMS_CARD_CAPACITY];
};

struct am_kms_session;

/* The C adapter owns every DRM, GBM, EGL, GLES, GEM handle, and DMA-BUF fd.
 * Rust passes initialized fixed-size records and writable frame storage only
 * for the duration of each synchronous call. */
int am_kms_probe(struct am_kms_info *output);
int am_kms_enumerate(struct am_kms_output *outputs, size_t capacity,
                     size_t *count);
struct am_kms_session *am_kms_open(const char *output_id, uint32_t *error);
int am_kms_session_geometry(struct am_kms_session *session,
                            struct am_kms_output *output);
/* Writes `width * height * 4` top-down BGRA bytes after revalidating the
 * selected active primary plane. The caller owns `destination`. */
int am_kms_read(struct am_kms_session *session, uint8_t *destination,
                size_t length, uint32_t *width, uint32_t *height);
void am_kms_close(struct am_kms_session *session);

#endif
