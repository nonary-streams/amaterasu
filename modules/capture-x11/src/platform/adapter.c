#include "adapter.h"

#include <xcb/randr.h>
#include <xcb/xcb.h>
#include <xcb/xproto.h>

#include <limits.h>
#include <stdlib.h>
#include <string.h>

enum {
    AM_X11_OK = 0,
    AM_X11_NO_DISPLAY = 1,
    AM_X11_CONNECT_FAILED = 2,
    AM_X11_RANDR_UNAVAILABLE = 3,
    AM_X11_OUTPUT_UNAVAILABLE = 4,
    AM_X11_INVALID_FORMAT = 5,
    AM_X11_READ_FAILED = 6,
    AM_X11_BUFFER_TOO_SMALL = 7,
};

struct am_x11_format {
    uint8_t bits_per_pixel;
    uint8_t scanline_pad;
    uint8_t image_byte_order;
    uint32_t red_mask;
    uint32_t green_mask;
    uint32_t blue_mask;
};

struct am_x11_session {
    xcb_connection_t *connection;
    xcb_window_t root;
    xcb_randr_output_t output_id;
    int32_t left;
    int32_t top;
    uint32_t width;
    uint32_t height;
    uint32_t root_width;
    uint32_t root_height;
    int screen_number;
    struct am_x11_format format;
};

struct am_x11_output_view {
    xcb_randr_output_t id;
    int32_t left;
    int32_t top;
    uint32_t width;
    uint32_t height;
    uint32_t rotation;
    char name[AM_X11_NAME_CAPACITY];
};

static xcb_connection_t *connect_display(const char *display, int *screen_number,
                                         uint32_t *error) {
    if (display == NULL || display[0] == '\0') {
        if (error != NULL) *error = AM_X11_NO_DISPLAY;
        return NULL;
    }
    xcb_connection_t *connection = xcb_connect(display, screen_number);
    if (connection == NULL || xcb_connection_has_error(connection) != 0) {
        if (connection != NULL) xcb_disconnect(connection);
        if (error != NULL) *error = AM_X11_CONNECT_FAILED;
        return NULL;
    }
    const xcb_query_extension_reply_t *extension =
        xcb_get_extension_data(connection, &xcb_randr_id);
    if (extension == NULL || !extension->present) {
        xcb_disconnect(connection);
        if (error != NULL) *error = AM_X11_RANDR_UNAVAILABLE;
        return NULL;
    }
    xcb_randr_query_version_cookie_t version_cookie =
        xcb_randr_query_version(connection, 1, 2);
    xcb_randr_query_version_reply_t *version =
        xcb_randr_query_version_reply(connection, version_cookie, NULL);
    if (version == NULL || version->major_version < 1 ||
        (version->major_version == 1 && version->minor_version < 2)) {
        free(version);
        xcb_disconnect(connection);
        if (error != NULL) *error = AM_X11_RANDR_UNAVAILABLE;
        return NULL;
    }
    free(version);
    return connection;
}

static xcb_screen_t *screen_for(xcb_connection_t *connection, int number) {
    const xcb_setup_t *setup = xcb_get_setup(connection);
    xcb_screen_iterator_t screens = xcb_setup_roots_iterator(setup);
    for (int index = 0; screens.rem > 0; ++index, xcb_screen_next(&screens)) {
        if (index == number) return screens.data;
    }
    return NULL;
}

static void safe_name(char output[AM_X11_NAME_CAPACITY], const uint8_t *name,
                      int length) {
    size_t destination = 0;
    for (int index = 0; index < length && destination + 1 < AM_X11_NAME_CAPACITY;
         ++index) {
        unsigned char byte = name[index];
        output[destination++] =
            (byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
                    (byte >= '0' && byte <= '9') || byte == '-' || byte == '_' ||
                    byte == '.'
                ? (char)byte
                : '_';
    }
    output[destination] = '\0';
}

static int output_view(xcb_connection_t *connection,
                       xcb_randr_get_screen_resources_current_reply_t *resources,
                       xcb_randr_output_t output_id, struct am_x11_output_view *output) {
    xcb_randr_get_output_info_cookie_t info_cookie =
        xcb_randr_get_output_info(connection, output_id, resources->config_timestamp);
    xcb_randr_get_output_info_reply_t *info =
        xcb_randr_get_output_info_reply(connection, info_cookie, NULL);
    if (info == NULL) return 0;
    int available = info->connection == XCB_RANDR_CONNECTION_CONNECTED &&
                    info->crtc != XCB_NONE && info->name_len > 0;
    if (!available) {
        free(info);
        return 0;
    }
    xcb_randr_get_crtc_info_cookie_t crtc_cookie =
        xcb_randr_get_crtc_info(connection, info->crtc, resources->config_timestamp);
    xcb_randr_get_crtc_info_reply_t *crtc =
        xcb_randr_get_crtc_info_reply(connection, crtc_cookie, NULL);
    if (crtc == NULL || crtc->mode == XCB_NONE || crtc->width == 0 ||
        crtc->height == 0 || crtc->rotation != XCB_RANDR_ROTATION_ROTATE_0) {
        free(crtc);
        free(info);
        return 0;
    }
    output->id = output_id;
    output->left = crtc->x;
    output->top = crtc->y;
    output->width = crtc->width;
    output->height = crtc->height;
    output->rotation = 1;
    safe_name(output->name, xcb_randr_get_output_info_name(info), info->name_len);
    free(crtc);
    free(info);
    return output->name[0] != '\0';
}

static int enumerate_connection(xcb_connection_t *connection, xcb_screen_t *screen,
                                struct am_x11_output_view *outputs,
                                size_t capacity, size_t *count) {
    xcb_randr_get_screen_resources_current_cookie_t cookie =
        xcb_randr_get_screen_resources_current(connection, screen->root);
    xcb_randr_get_screen_resources_current_reply_t *resources =
        xcb_randr_get_screen_resources_current_reply(connection, cookie, NULL);
    if (resources == NULL) return AM_X11_CONNECT_FAILED;
    xcb_randr_output_t *output_ids = xcb_randr_get_screen_resources_current_outputs(resources);
    int output_count = xcb_randr_get_screen_resources_current_outputs_length(resources);
    size_t found = 0;
    for (int index = 0; index < output_count; ++index) {
        struct am_x11_output_view candidate;
        memset(&candidate, 0, sizeof(candidate));
        if (!output_view(connection, resources, output_ids[index], &candidate)) continue;
        if (found < capacity && outputs != NULL) outputs[found] = candidate;
        ++found;
    }
    free(resources);
    *count = found;
    if (found > capacity) return AM_X11_BUFFER_TOO_SMALL;
    return xcb_connection_has_error(connection) == 0 ? AM_X11_OK : AM_X11_CONNECT_FAILED;
}

static const xcb_visualtype_t *root_visual(const xcb_screen_t *screen) {
    xcb_depth_iterator_t depths = xcb_screen_allowed_depths_iterator(screen);
    while (depths.rem > 0) {
        xcb_visualtype_iterator_t visuals = xcb_depth_visuals_iterator(depths.data);
        while (visuals.rem > 0) {
            if (visuals.data->visual_id == screen->root_visual) return visuals.data;
            xcb_visualtype_next(&visuals);
        }
        xcb_depth_next(&depths);
    }
    return NULL;
}

static int contiguous_mask(uint32_t mask) {
    if (mask == 0) return 0;
    while ((mask & 1U) == 0) mask >>= 1;
    return (mask & (mask + 1U)) == 0;
}

static int root_format(xcb_connection_t *connection, xcb_screen_t *screen,
                       struct am_x11_format *format) {
    const xcb_setup_t *setup = xcb_get_setup(connection);
    const xcb_visualtype_t *visual = root_visual(screen);
    if (visual == NULL || visual->_class != XCB_VISUAL_CLASS_TRUE_COLOR ||
        !contiguous_mask(visual->red_mask) || !contiguous_mask(visual->green_mask) ||
        !contiguous_mask(visual->blue_mask) ||
        (visual->red_mask & visual->green_mask) != 0 ||
        (visual->red_mask & visual->blue_mask) != 0 ||
        (visual->green_mask & visual->blue_mask) != 0) {
        return AM_X11_INVALID_FORMAT;
    }
    xcb_format_iterator_t formats = xcb_setup_pixmap_formats_iterator(setup);
    while (formats.rem > 0 && formats.data->depth != screen->root_depth) {
        xcb_format_next(&formats);
    }
    if (formats.rem <= 0 || formats.data->bits_per_pixel == 0 ||
        formats.data->bits_per_pixel > 32 || formats.data->bits_per_pixel % 8 != 0 ||
        formats.data->scanline_pad == 0) {
        return AM_X11_INVALID_FORMAT;
    }
    format->bits_per_pixel = formats.data->bits_per_pixel;
    format->scanline_pad = formats.data->scanline_pad;
    format->image_byte_order = setup->image_byte_order;
    format->red_mask = visual->red_mask;
    format->green_mask = visual->green_mask;
    format->blue_mask = visual->blue_mask;
    return AM_X11_OK;
}

int am_x11_probe(const char *display, struct am_x11_info *output) {
    if (output == NULL) return AM_X11_CONNECT_FAILED;
    memset(output, 0, sizeof(*output));
    uint32_t error = AM_X11_OK;
    int screen_number = 0;
    xcb_connection_t *connection = connect_display(display, &screen_number, &error);
    if (connection == NULL) {
        output->error = error;
        return (int)error;
    }
    xcb_screen_t *screen = screen_for(connection, screen_number);
    size_t count = 0;
    struct am_x11_format format = {0};
    int result = screen == NULL
                     ? AM_X11_CONNECT_FAILED
                     : root_format(connection, screen, &format);
    if (result == AM_X11_OK) {
        result = enumerate_connection(connection, screen, NULL, 0, &count);
    }
    output->root_width = screen == NULL ? 0 : screen->width_in_pixels;
    output->root_height = screen == NULL ? 0 : screen->height_in_pixels;
    output->available = result == AM_X11_BUFFER_TOO_SMALL || (result == AM_X11_OK && count > 0);
    output->error = output->available ? AM_X11_OK
                                      : (result == AM_X11_OK ? AM_X11_OUTPUT_UNAVAILABLE
                                                             : (uint32_t)result);
    xcb_disconnect(connection);
    return (int)output->error;
}

int am_x11_enumerate(const char *display, struct am_x11_output *outputs,
                     size_t capacity, size_t *count, uint32_t *root_width,
                     uint32_t *root_height) {
    if (count == NULL || root_width == NULL || root_height == NULL ||
        (capacity != 0 && outputs == NULL)) return AM_X11_CONNECT_FAILED;
    *count = 0;
    *root_width = 0;
    *root_height = 0;
    int screen_number = 0;
    uint32_t error = AM_X11_OK;
    xcb_connection_t *connection = connect_display(display, &screen_number, &error);
    if (connection == NULL) return (int)error;
    xcb_screen_t *screen = screen_for(connection, screen_number);
    int result;
    if (screen == NULL) {
        result = AM_X11_CONNECT_FAILED;
    } else {
        *root_width = screen->width_in_pixels;
        *root_height = screen->height_in_pixels;
        size_t needed = 0;
        result = enumerate_connection(connection, screen, NULL, 0, &needed);
        if (result == AM_X11_BUFFER_TOO_SMALL) {
            if (needed > capacity) {
                *count = needed;
                result = AM_X11_BUFFER_TOO_SMALL;
            } else {
                struct am_x11_output_view *views =
                    calloc(needed == 0 ? 1 : needed, sizeof(*views));
                if (views == NULL) {
                    result = AM_X11_CONNECT_FAILED;
                } else {
                    result = enumerate_connection(connection, screen, views, needed, &needed);
                    if (result == AM_X11_OK) {
                        for (size_t index = 0; index < needed; ++index) {
                            outputs[index].output_id = views[index].id;
                            outputs[index].left = views[index].left;
                            outputs[index].top = views[index].top;
                            outputs[index].width = views[index].width;
                            outputs[index].height = views[index].height;
                            outputs[index].rotation = views[index].rotation;
                            memcpy(outputs[index].name, views[index].name,
                                   sizeof(outputs[index].name));
                        }
                        *count = needed;
                    }
                    free(views);
                }
            }
        }
    }
    xcb_disconnect(connection);
    return result;
}

struct am_x11_session *am_x11_open(const char *display, uint32_t output_id,
                                   const char *expected_name, uint32_t *error) {
    if (error == NULL || output_id == 0 || expected_name == NULL ||
        expected_name[0] == '\0') return NULL;
    *error = AM_X11_OK;
    int screen_number = 0;
    xcb_connection_t *connection = connect_display(display, &screen_number, error);
    if (connection == NULL) return NULL;
    xcb_screen_t *screen = screen_for(connection, screen_number);
    if (screen == NULL) {
        *error = AM_X11_CONNECT_FAILED;
        xcb_disconnect(connection);
        return NULL;
    }
    size_t count = 0;
    int list_result = enumerate_connection(connection, screen, NULL, 0, &count);
    if (list_result != AM_X11_BUFFER_TOO_SMALL || count == 0 || count > 64) {
        *error = list_result == AM_X11_OK ? AM_X11_OUTPUT_UNAVAILABLE : (uint32_t)list_result;
        xcb_disconnect(connection);
        return NULL;
    }
    struct am_x11_output_view *outputs = calloc(count, sizeof(*outputs));
    if (outputs == NULL) {
        *error = AM_X11_CONNECT_FAILED;
        xcb_disconnect(connection);
        return NULL;
    }
    int result = enumerate_connection(connection, screen, outputs, count, &count);
    struct am_x11_output_view *selected = NULL;
    for (size_t index = 0; result == AM_X11_OK && index < count; ++index) {
        if (outputs[index].id == output_id &&
            strcmp(outputs[index].name, expected_name) == 0) {
            selected = &outputs[index];
            break;
        }
    }
    if (result != AM_X11_OK || selected == NULL) {
        *error = result == AM_X11_OK ? AM_X11_OUTPUT_UNAVAILABLE : (uint32_t)result;
        free(outputs);
        xcb_disconnect(connection);
        return NULL;
    }
    struct am_x11_session *session = calloc(1, sizeof(*session));
    if (session == NULL) {
        *error = AM_X11_CONNECT_FAILED;
        free(outputs);
        xcb_disconnect(connection);
        return NULL;
    }
    session->connection = connection;
    session->root = screen->root;
    session->output_id = selected->id;
    session->left = selected->left;
    session->top = selected->top;
    session->width = selected->width;
    session->height = selected->height;
    session->root_width = screen->width_in_pixels;
    session->root_height = screen->height_in_pixels;
    session->screen_number = screen_number;
    session->format = (struct am_x11_format){0};
    *error = (uint32_t)root_format(connection, screen, &session->format);
    free(outputs);
    if (*error != AM_X11_OK) {
        xcb_disconnect(connection);
        free(session);
        return NULL;
    }
    return session;
}

int am_x11_session_geometry(struct am_x11_session *session,
                            struct am_x11_output *output,
                            uint32_t *root_width, uint32_t *root_height) {
    if (session == NULL || output == NULL || root_width == NULL || root_height == NULL) {
        return AM_X11_CONNECT_FAILED;
    }
    memset(output, 0, sizeof(*output));
    output->output_id = session->output_id;
    output->left = session->left;
    output->top = session->top;
    output->width = session->width;
    output->height = session->height;
    output->rotation = 1;
    *root_width = session->root_width;
    *root_height = session->root_height;
    return AM_X11_OK;
}

static unsigned int mask_shift(uint32_t mask) {
    unsigned int shift = 0;
    while ((mask & 1U) == 0) {
        mask >>= 1;
        ++shift;
    }
    return shift;
}

static uint8_t channel(uint32_t pixel, uint32_t mask) {
    unsigned int shift = mask_shift(mask);
    uint32_t maximum = mask >> shift;
    uint64_t value = (pixel & mask) >> shift;
    return (uint8_t)((value * 255U + maximum / 2U) / maximum);
}

int am_x11_read(struct am_x11_session *session, uint8_t *destination,
                size_t length, uint32_t *width, uint32_t *height) {
    if (session == NULL || destination == NULL || width == NULL || height == NULL) {
        return AM_X11_READ_FAILED;
    }
    size_t stride = (size_t)session->width * 4U;
    if (session->width == 0 || session->height == 0 ||
        session->width > 8192 || session->height > 8192 ||
        stride > SIZE_MAX / session->height || length != stride * session->height ||
        session->left < 0 || session->top < 0 || session->left > INT16_MAX ||
        session->top > INT16_MAX || session->width > UINT16_MAX ||
        session->height > UINT16_MAX ||
        (uint64_t)(uint32_t)session->left + session->width > session->root_width ||
        (uint64_t)(uint32_t)session->top + session->height > session->root_height) {
        return AM_X11_OUTPUT_UNAVAILABLE;
    }
    xcb_screen_t *screen = screen_for(session->connection, session->screen_number);
    if (screen == NULL || screen->root != session->root ||
        screen->width_in_pixels != session->root_width ||
        screen->height_in_pixels != session->root_height) {
        return AM_X11_OUTPUT_UNAVAILABLE;
    }
    size_t current_count = 0;
    int list_result = enumerate_connection(session->connection,
                                           screen,
                                           NULL, 0, &current_count);
    if (list_result != AM_X11_BUFFER_TOO_SMALL || current_count == 0 || current_count > 64) {
        return AM_X11_OUTPUT_UNAVAILABLE;
    }
    struct am_x11_output_view *outputs = calloc(current_count, sizeof(*outputs));
    if (outputs == NULL) return AM_X11_READ_FAILED;
    list_result = enumerate_connection(session->connection,
                                       screen,
                                       outputs, current_count, &current_count);
    int same_mode = 0;
    for (size_t index = 0; list_result == AM_X11_OK && index < current_count; ++index) {
        if (outputs[index].id == session->output_id &&
            outputs[index].left == session->left && outputs[index].top == session->top &&
            outputs[index].width == session->width &&
            outputs[index].height == session->height) {
            same_mode = 1;
            break;
        }
    }
    free(outputs);
    if (!same_mode) return AM_X11_OUTPUT_UNAVAILABLE;

    xcb_get_image_cookie_t cookie = xcb_get_image(
        session->connection, XCB_IMAGE_FORMAT_Z_PIXMAP, session->root,
        (int16_t)session->left, (int16_t)session->top, (uint16_t)session->width,
        (uint16_t)session->height, UINT32_MAX);
    xcb_get_image_reply_t *image = xcb_get_image_reply(session->connection, cookie, NULL);
    if (image == NULL || xcb_connection_has_error(session->connection) != 0) {
        free(image);
        return AM_X11_READ_FAILED;
    }
    size_t pixel_bytes = session->format.bits_per_pixel / 8U;
    size_t row_bits = (size_t)session->width * session->format.bits_per_pixel;
    size_t pad_bits = session->format.scanline_pad;
    size_t row_bytes = ((row_bits + pad_bits - 1U) / pad_bits) * (pad_bits / 8U);
    size_t required = row_bytes * session->height;
    if (pixel_bytes == 0 || pixel_bytes > 4 || row_bytes < (size_t)session->width * pixel_bytes ||
        required > (size_t)xcb_get_image_data_length(image)) {
        free(image);
        return AM_X11_INVALID_FORMAT;
    }
    const uint8_t *source = xcb_get_image_data(image);
    for (uint32_t y = 0; y < session->height; ++y) {
        const uint8_t *row = source + (size_t)y * row_bytes;
        uint8_t *target = destination + (size_t)y * stride;
        for (uint32_t x = 0; x < session->width; ++x) {
            const uint8_t *pixel_bytes_source = row + (size_t)x * pixel_bytes;
            uint32_t pixel = 0;
            for (size_t byte = 0; byte < pixel_bytes; ++byte) {
                size_t shift = session->format.image_byte_order == XCB_IMAGE_ORDER_LSB_FIRST
                                   ? byte * 8U
                                   : (pixel_bytes - byte - 1U) * 8U;
                pixel |= (uint32_t)pixel_bytes_source[byte] << shift;
            }
            target[(size_t)x * 4U + 0] = channel(pixel, session->format.blue_mask);
            target[(size_t)x * 4U + 1] = channel(pixel, session->format.green_mask);
            target[(size_t)x * 4U + 2] = channel(pixel, session->format.red_mask);
            target[(size_t)x * 4U + 3] = 255;
        }
    }
    free(image);
    *width = session->width;
    *height = session->height;
    return AM_X11_OK;
}

void am_x11_close(struct am_x11_session *session) {
    if (session == NULL) return;
    xcb_disconnect(session->connection);
    free(session);
}
