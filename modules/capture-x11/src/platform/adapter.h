#ifndef AMATERASU_X11_ADAPTER_H
#define AMATERASU_X11_ADAPTER_H

#include <stddef.h>
#include <stdint.h>

#define AM_X11_NAME_CAPACITY 128

struct am_x11_output {
    uint32_t output_id;
    int32_t left;
    int32_t top;
    uint32_t width;
    uint32_t height;
    uint32_t rotation;
    char name[AM_X11_NAME_CAPACITY];
};

struct am_x11_info {
    uint32_t available;
    uint32_t root_width;
    uint32_t root_height;
    uint32_t error;
};

struct am_x11_session;

/* All functions keep XCB objects private to this adapter. The display string
 * is supplied explicitly; a missing DISPLAY never falls back to :0. */
int am_x11_probe(const char *display, struct am_x11_info *output);
int am_x11_enumerate(const char *display, struct am_x11_output *outputs,
                     size_t capacity, size_t *count, uint32_t *root_width,
                     uint32_t *root_height);
struct am_x11_session *am_x11_open(const char *display, uint32_t output_id,
                                   const char *expected_name, uint32_t *error);
int am_x11_session_geometry(struct am_x11_session *session,
                            struct am_x11_output *output,
                            uint32_t *root_width, uint32_t *root_height);
/* Writes exactly width*height BGRA bytes after revalidating selected output
 * identity and mode. Caller owns a writable destination of `length` bytes. */
int am_x11_read(struct am_x11_session *session, uint8_t *destination,
                size_t length, uint32_t *width, uint32_t *height);
void am_x11_close(struct am_x11_session *session);

#endif
