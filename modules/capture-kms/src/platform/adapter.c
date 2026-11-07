#define _GNU_SOURCE
#include "adapter.h"

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <drm_fourcc.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#define AM_KMS_MAX_DIMENSION 8192U
#define AM_KMS_MAX_FRAME_BYTES (128U * 1024U * 1024U)
#define AM_KMS_INVALID_MODIFIER UINT64_MAX

struct am_kms_framebuffer {
    uint32_t id;
    uint32_t width;
    uint32_t height;
    uint32_t fourcc;
    uint32_t pitch;
    uint32_t offset;
    uint32_t handle;
    uint64_t modifier;
    int dma_buf_fd;
};

struct am_kms_card {
    int fd;
    char path[AM_KMS_CARD_CAPACITY];
    char driver[64];
};

struct am_kms_session {
    struct am_kms_card card;
    struct am_kms_output output;
    uint32_t connector_id;
    uint32_t crtc_id;
    uint32_t plane_id;
    uint32_t framebuffer_id;
    struct am_kms_framebuffer framebuffer;
    struct gbm_device *gbm;
    EGLDisplay display;
    EGLContext context;
    EGLSurface surface;
    PFNEGLCREATEIMAGEKHRPROC create_image;
    PFNEGLDESTROYIMAGEKHRPROC destroy_image;
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_to_texture;
    EGLImageKHR image;
    GLuint texture;
    GLuint framebuffer_object;
    bool egl_initialized;
};

static int error_from_errno(int error) {
    if (error == EACCES || error == EPERM) {
        return AM_KMS_ACCESS_DENIED;
    }
    if (error == ENOENT || error == ENODEV) {
        return AM_KMS_NO_DEVICE;
    }
    return AM_KMS_CAPTURE_FAILED;
}

static bool has_rgb32_format(uint32_t fourcc) {
    return fourcc == DRM_FORMAT_XRGB8888 || fourcc == DRM_FORMAT_ARGB8888 ||
           fourcc == DRM_FORMAT_XBGR8888 || fourcc == DRM_FORMAT_ABGR8888;
}

static void close_gem_handle(int fd, uint32_t handle) {
    if (fd < 0 || handle == 0) {
        return;
    }
    struct drm_gem_close request = {0};
    request.handle = handle;
    (void)drmIoctl(fd, DRM_IOCTL_GEM_CLOSE, &request);
}

static bool add_gem_handle(uint32_t handles[4], size_t *count, uint32_t handle) {
    if (handle == 0) {
        return true;
    }
    for (size_t index = 0; index < *count; ++index) {
        if (handles[index] == handle) {
            return true;
        }
    }
    if (*count == 4) {
        return false;
    }
    handles[(*count)++] = handle;
    return true;
}

static void close_gem_handles(int fd, const uint32_t handles[4], size_t count) {
    for (size_t index = 0; index < count; ++index) {
        close_gem_handle(fd, handles[index]);
    }
}

/* drmModeGetFB2 returns file-local GEM handles that this adapter must close.
 * The PRIME fd takes its own reference, and remains owned until EGL teardown. */
static int export_framebuffer(int fd, uint32_t id,
                              struct am_kms_framebuffer *output) {
    if (output == NULL || fd < 0 || id == 0) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    memset(output, 0, sizeof(*output));
    output->dma_buf_fd = -1;
    output->modifier = AM_KMS_INVALID_MODIFIER;

    uint32_t handles[4] = {0, 0, 0, 0};
    size_t handle_count = 0;
    int result = AM_KMS_UNSUPPORTED;
    drmModeFB2Ptr fb2 = drmModeGetFB2(fd, id);
    if (fb2 != NULL) {
        output->id = fb2->fb_id;
        output->width = fb2->width;
        output->height = fb2->height;
        output->fourcc = fb2->pixel_format;
        output->pitch = fb2->pitches[0];
        output->offset = fb2->offsets[0];
        output->handle = fb2->handles[0];
        if ((fb2->flags & DRM_MODE_FB_MODIFIERS) != 0) {
            output->modifier = fb2->modifier;
        }
        for (size_t index = 0; index < 4; ++index) {
            if (!add_gem_handle(handles, &handle_count, fb2->handles[index])) {
                drmModeFreeFB2(fb2);
                close_gem_handles(fd, handles, handle_count);
                return AM_KMS_UNSUPPORTED;
            }
        }
        drmModeFreeFB2(fb2);
    } else {
        drmModeFBPtr fb = drmModeGetFB(fd, id);
        if (fb == NULL) {
            return error_from_errno(errno);
        }
        output->id = fb->fb_id;
        output->width = fb->width;
        output->height = fb->height;
        output->fourcc = DRM_FORMAT_XRGB8888;
        output->pitch = fb->pitch;
        output->handle = fb->handle;
        output->modifier = AM_KMS_INVALID_MODIFIER;
        if (fb->bpp == 32 && fb->depth == 24 && add_gem_handle(handles, &handle_count, fb->handle)) {
            result = AM_KMS_OK;
        }
        drmModeFreeFB(fb);
        if (result != AM_KMS_OK) {
            close_gem_handles(fd, handles, handle_count);
            return AM_KMS_UNSUPPORTED;
        }
    }

    const bool layout_valid = output->width > 0 && output->height > 0 &&
                              output->width <= AM_KMS_MAX_DIMENSION &&
                              output->height <= AM_KMS_MAX_DIMENSION &&
                              output->pitch >= output->width * 4U &&
                              output->pitch <= INT_MAX && output->offset <= INT_MAX &&
                              (uint64_t)output->pitch * output->height <= AM_KMS_MAX_FRAME_BYTES &&
                              output->handle != 0 && has_rgb32_format(output->fourcc);
    if (layout_valid) {
        int dma_buf_fd = -1;
        if (drmPrimeHandleToFD(fd, output->handle, DRM_CLOEXEC, &dma_buf_fd) == 0 &&
            dma_buf_fd >= 0) {
            output->dma_buf_fd = dma_buf_fd;
            result = AM_KMS_OK;
        } else {
            result = error_from_errno(errno);
        }
    }
    close_gem_handles(fd, handles, handle_count);
    if (result != AM_KMS_OK && output->dma_buf_fd >= 0) {
        close(output->dma_buf_fd);
        output->dma_buf_fd = -1;
    }
    return result;
}

static void close_framebuffer(struct am_kms_framebuffer *framebuffer) {
    if (framebuffer != NULL && framebuffer->dma_buf_fd >= 0) {
        close(framebuffer->dma_buf_fd);
        framebuffer->dma_buf_fd = -1;
    }
}

static int open_card(const char *path, struct am_kms_card *output) {
    if (path == NULL || output == NULL) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    memset(output, 0, sizeof(*output));
    output->fd = -1;
    if (strlen(path) >= sizeof(output->path)) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        return error_from_errno(errno);
    }
    drmVersionPtr version = drmGetVersion(fd);
    if (version == NULL || version->name == NULL || version->name_len <= 0 ||
        (size_t)version->name_len >= sizeof(output->driver)) {
        if (version != NULL) {
            drmFreeVersion(version);
        }
        close(fd);
        return AM_KMS_UNSUPPORTED;
    }
    memcpy(output->driver, version->name, (size_t)version->name_len);
    output->driver[version->name_len] = '\0';
    drmFreeVersion(version);
    /* Managed Vibeshine outputs require its private frame and presentation ABI. */
    if (strcmp(output->driver, "vibeshine_drm") == 0) {
        close(fd);
        return AM_KMS_UNSUPPORTED;
    }
    if (drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1) != 0) {
        int error = error_from_errno(errno);
        close(fd);
        return error;
    }
    output->fd = fd;
    memcpy(output->path, path, strlen(path) + 1);
    return AM_KMS_OK;
}

static void close_card(struct am_kms_card *card) {
    if (card != NULL && card->fd >= 0) {
        close(card->fd);
        card->fd = -1;
    }
}

static bool is_primary_plane(int fd, uint32_t plane_id) {
    drmModeObjectPropertiesPtr properties =
        drmModeObjectGetProperties(fd, plane_id, DRM_MODE_OBJECT_PLANE);
    if (properties == NULL) {
        return false;
    }
    bool primary = false;
    for (uint32_t index = 0; index < properties->count_props; ++index) {
        drmModePropertyPtr property = drmModeGetProperty(fd, properties->props[index]);
        if (property != NULL) {
            if (strcmp(property->name, "type") == 0) {
                primary = properties->prop_values[index] == DRM_PLANE_TYPE_PRIMARY;
                drmModeFreeProperty(property);
                break;
            }
            drmModeFreeProperty(property);
        }
    }
    drmModeFreeObjectProperties(properties);
    return primary;
}

static bool connector_matches_crtc(int fd, drmModeResPtr resources,
                                   uint32_t crtc_id, uint32_t *connector_id,
                                   char name[AM_KMS_NAME_CAPACITY]) {
    if (resources == NULL || connector_id == NULL || name == NULL) {
        return false;
    }
    name[0] = '\0';
    for (int index = 0; index < resources->count_connectors; ++index) {
        drmModeConnectorPtr connector =
            drmModeGetConnectorCurrent(fd, resources->connectors[index]);
        if (connector == NULL) {
            continue;
        }
        bool matches = false;
        if (connector->connection == DRM_MODE_CONNECTED && connector->encoder_id != 0) {
            drmModeEncoderPtr encoder = drmModeGetEncoder(fd, connector->encoder_id);
            matches = encoder != NULL && encoder->crtc_id == crtc_id;
            if (encoder != NULL) {
                drmModeFreeEncoder(encoder);
            }
        }
        if (matches) {
            const char *type = drmModeGetConnectorTypeName(connector->connector_type);
            if (type != NULL) {
                int written = snprintf(name, AM_KMS_NAME_CAPACITY, "%s-%u", type,
                                      connector->connector_type_id);
                if (written > 0 && (size_t)written < AM_KMS_NAME_CAPACITY) {
                    *connector_id = connector->connector_id;
                    drmModeFreeConnector(connector);
                    return true;
                }
            }
        }
        drmModeFreeConnector(connector);
    }
    return false;
}

static int format_output_id(const char *card, uint32_t connector_id,
                            uint32_t crtc_id, uint32_t plane_id,
                            char output[AM_KMS_ID_CAPACITY]) {
    const char *base = strrchr(card, '/');
    base = base == NULL ? card : base + 1;
    int written = snprintf(output, AM_KMS_ID_CAPACITY,
                           "kms:%s/conn=%u/crtc=%u/plane=%u", base,
                           connector_id, crtc_id, plane_id);
    return written > 0 && written < AM_KMS_ID_CAPACITY ? AM_KMS_OK : AM_KMS_UNSUPPORTED;
}

static bool parse_decimal_u32(const char **cursor, uint32_t *output) {
    if (cursor == NULL || *cursor == NULL || output == NULL ||
        **cursor < '0' || **cursor > '9') {
        return false;
    }
    if (**cursor == '0' && (*cursor)[1] >= '0' && (*cursor)[1] <= '9') {
        return false;
    }
    uint32_t value = 0;
    do {
        uint32_t digit = (uint32_t)(**cursor - '0');
        if (value > (UINT32_MAX - digit) / 10U) {
            return false;
        }
        value = value * 10U + digit;
        ++(*cursor);
    } while (**cursor >= '0' && **cursor <= '9');
    *output = value;
    return true;
}

static bool consume_literal(const char **cursor, const char *literal) {
    size_t length = strlen(literal);
    if (cursor == NULL || *cursor == NULL || strncmp(*cursor, literal, length) != 0) {
        return false;
    }
    *cursor += length;
    return true;
}

static int parse_output_id(const char *id, char card[AM_KMS_CARD_CAPACITY],
                           uint32_t *connector_id, uint32_t *crtc_id,
                           uint32_t *plane_id) {
    if (id == NULL || card == NULL || connector_id == NULL || crtc_id == NULL ||
        plane_id == NULL || strnlen(id, AM_KMS_ID_CAPACITY) == AM_KMS_ID_CAPACITY) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    const char *cursor = id;
    uint32_t card_index = 0;
    uint32_t connector = 0;
    uint32_t crtc = 0;
    uint32_t plane = 0;
    if (!consume_literal(&cursor, "kms:card") ||
        !parse_decimal_u32(&cursor, &card_index) ||
        !consume_literal(&cursor, "/conn=") ||
        !parse_decimal_u32(&cursor, &connector) || connector == 0 ||
        !consume_literal(&cursor, "/crtc=") ||
        !parse_decimal_u32(&cursor, &crtc) || crtc == 0 ||
        !consume_literal(&cursor, "/plane=") ||
        !parse_decimal_u32(&cursor, &plane) || plane == 0 || *cursor != '\0') {
        return AM_KMS_INVALID_ARGUMENT;
    }
    int written = snprintf(card, AM_KMS_CARD_CAPACITY, "card%u", card_index);
    if (written <= 0 || written >= AM_KMS_CARD_CAPACITY) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    *connector_id = connector;
    *crtc_id = crtc;
    *plane_id = plane;
    return AM_KMS_OK;
}

static void fill_output(struct am_kms_output *output, const struct am_kms_card *card,
                        uint32_t connector_id, uint32_t crtc_id,
                        uint32_t plane_id, const char *name,
                        const drmModeCrtcPtr crtc,
                        const struct am_kms_framebuffer *framebuffer,
                        int framebuffer_status) {
    memset(output, 0, sizeof(*output));
    output->left = (int32_t)crtc->x;
    output->top = (int32_t)crtc->y;
    output->width = crtc->width;
    output->height = crtc->height;
    output->rotation = DRM_MODE_ROTATE_0;
    output->attached = 1;
    output->framebuffer_export_supported = framebuffer_status == AM_KMS_OK ? 1U : 0U;
    output->error = (uint32_t)framebuffer_status;
    memcpy(output->name, name, strnlen(name, sizeof(output->name) - 1));
    memcpy(output->card, card->path, strnlen(card->path, sizeof(output->card) - 1));
    if (format_output_id(card->path, connector_id, crtc_id, plane_id, output->id) != AM_KMS_OK) {
        output->framebuffer_export_supported = 0;
        output->error = AM_KMS_UNSUPPORTED;
    }
    if (framebuffer_status == AM_KMS_OK && framebuffer != NULL &&
        (framebuffer->width != crtc->width || framebuffer->height != crtc->height)) {
        output->framebuffer_export_supported = 0;
        output->error = AM_KMS_UNSUPPORTED;
    }
}

static bool plane_geometry_supported(int fd, uint32_t plane_id,
                                     const struct am_kms_framebuffer *fb,
                                     const drmModeCrtcPtr crtc) {
    drmModeObjectPropertiesPtr properties =
        drmModeObjectGetProperties(fd, plane_id, DRM_MODE_OBJECT_PLANE);
    if (properties == NULL) {
        return false;
    }
    bool have_source_x = false;
    bool have_source_y = false;
    bool have_source_width = false;
    bool have_source_height = false;
    bool have_target_x = false;
    bool have_target_y = false;
    bool have_target_width = false;
    bool have_target_height = false;
    bool have_rotation = false;
    uint64_t source_x = 0;
    uint64_t source_y = 0;
    uint64_t source_width = 0;
    uint64_t source_height = 0;
    uint64_t target_x = 0;
    uint64_t target_y = 0;
    uint64_t target_width = 0;
    uint64_t target_height = 0;
    uint64_t rotation = DRM_MODE_ROTATE_0;
    for (uint32_t index = 0; index < properties->count_props; ++index) {
        drmModePropertyPtr property = drmModeGetProperty(fd, properties->props[index]);
        if (property == NULL) {
            continue;
        }
        uint64_t value = properties->prop_values[index];
        if (strcmp(property->name, "SRC_X") == 0) {
            source_x = value;
            have_source_x = true;
        } else if (strcmp(property->name, "SRC_Y") == 0) {
            source_y = value;
            have_source_y = true;
        } else if (strcmp(property->name, "SRC_W") == 0) {
            source_width = value;
            have_source_width = true;
        } else if (strcmp(property->name, "SRC_H") == 0) {
            source_height = value;
            have_source_height = true;
        } else if (strcmp(property->name, "CRTC_X") == 0) {
            target_x = value;
            have_target_x = true;
        } else if (strcmp(property->name, "CRTC_Y") == 0) {
            target_y = value;
            have_target_y = true;
        } else if (strcmp(property->name, "CRTC_W") == 0) {
            target_width = value;
            have_target_width = true;
        } else if (strcmp(property->name, "CRTC_H") == 0) {
            target_height = value;
            have_target_height = true;
        } else if (strcmp(property->name, "rotation") == 0) {
            rotation = value;
            have_rotation = true;
        }
        drmModeFreeProperty(property);
    }
    drmModeFreeObjectProperties(properties);

    if (have_rotation && rotation != DRM_MODE_ROTATE_0) {
        return false;
    }
    const bool any_source = have_source_x || have_source_y || have_source_width ||
                            have_source_height || have_target_x || have_target_y ||
                            have_target_width || have_target_height;
    if (any_source) {
        if (!(have_source_x && have_source_y && have_source_width && have_source_height &&
              have_target_x && have_target_y && have_target_width && have_target_height)) {
            return false;
        }
        if (source_x != 0 || source_y != 0 || target_x != 0 || target_y != 0 ||
            source_width != ((uint64_t)fb->width << 16) ||
            source_height != ((uint64_t)fb->height << 16) ||
            target_width != crtc->width || target_height != crtc->height) {
            return false;
        }
        return true;
    }
    return false;
}

static int append_output(struct am_kms_output outputs[AM_KMS_MAX_OUTPUTS],
                         size_t *count, const struct am_kms_output *output) {
    if (*count >= AM_KMS_MAX_OUTPUTS) {
        return AM_KMS_TOO_MANY_OUTPUTS;
    }
    outputs[(*count)++] = *output;
    return AM_KMS_OK;
}

static int compare_outputs(const void *left, const void *right) {
    const struct am_kms_output *a = left;
    const struct am_kms_output *b = right;
    return strcmp(a->id, b->id);
}

static int scan_card(const char *path,
                     struct am_kms_output outputs[AM_KMS_MAX_OUTPUTS],
                     size_t *count) {
    struct am_kms_card card;
    int open_result = open_card(path, &card);
    if (open_result != AM_KMS_OK) {
        return open_result;
    }
    drmModeResPtr resources = drmModeGetResources(card.fd);
    drmModePlaneResPtr planes = drmModeGetPlaneResources(card.fd);
    if (resources == NULL || planes == NULL) {
        drmModeFreeResources(resources);
        drmModeFreePlaneResources(planes);
        close_card(&card);
        return error_from_errno(errno);
    }
    size_t before = *count;
    int result = AM_KMS_OK;
    for (uint32_t plane_index = 0; plane_index < planes->count_planes; ++plane_index) {
        drmModePlanePtr plane = drmModeGetPlane(card.fd, planes->planes[plane_index]);
        if (plane == NULL) {
            continue;
        }
        if (plane->fb_id == 0 || plane->crtc_id == 0 ||
            !is_primary_plane(card.fd, plane->plane_id)) {
            drmModeFreePlane(plane);
            continue;
        }
        uint32_t connector_id = 0;
        char name[AM_KMS_NAME_CAPACITY] = {0};
        if (!connector_matches_crtc(card.fd, resources, plane->crtc_id,
                                    &connector_id, name)) {
            drmModeFreePlane(plane);
            continue;
        }
        drmModeCrtcPtr crtc = drmModeGetCrtc(card.fd, plane->crtc_id);
        if (crtc == NULL || crtc->mode_valid == 0 || crtc->width == 0 ||
            crtc->height == 0 || crtc->width > AM_KMS_MAX_DIMENSION ||
            crtc->height > AM_KMS_MAX_DIMENSION) {
            drmModeFreeCrtc(crtc);
            drmModeFreePlane(plane);
            continue;
        }
        struct am_kms_framebuffer framebuffer;
        int framebuffer_status = export_framebuffer(card.fd, plane->fb_id, &framebuffer);
        if (framebuffer_status == AM_KMS_OK &&
            !plane_geometry_supported(card.fd, plane->plane_id, &framebuffer, crtc)) {
            framebuffer_status = AM_KMS_UNSUPPORTED;
        }
        struct am_kms_output output;
        fill_output(&output, &card, connector_id, plane->crtc_id,
                    plane->plane_id, name, crtc, &framebuffer,
                    framebuffer_status);
        close_framebuffer(&framebuffer);
        result = append_output(outputs, count, &output);
        drmModeFreeCrtc(crtc);
        drmModeFreePlane(plane);
        if (result != AM_KMS_OK) {
            break;
        }
    }
    drmModeFreePlaneResources(planes);
    drmModeFreeResources(resources);
    close_card(&card);
    if (result != AM_KMS_OK) {
        return result;
    }
    return *count > before ? AM_KMS_OK : AM_KMS_NO_OUTPUT;
}

static bool is_card_name(const char *name) {
    if (name == NULL || strncmp(name, "card", 4) != 0 || name[4] == '\0') {
        return false;
    }
    for (const char *cursor = name + 4; *cursor != '\0'; ++cursor) {
        if (*cursor < '0' || *cursor > '9') {
            return false;
        }
    }
    return true;
}

static int collect_card_paths(char paths[64][AM_KMS_CARD_CAPACITY], size_t *count) {
    if (count == NULL) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    *count = 0;
    DIR *directory = opendir("/dev/dri");
    if (directory == NULL) {
        return error_from_errno(errno);
    }
    struct dirent *entry;
    while ((entry = readdir(directory)) != NULL) {
        if (!is_card_name(entry->d_name)) {
            continue;
        }
        if (*count >= 64) {
            closedir(directory);
            return AM_KMS_TOO_MANY_OUTPUTS;
        }
        int written = snprintf(paths[*count], AM_KMS_CARD_CAPACITY,
                               "/dev/dri/%s", entry->d_name);
        if (written <= 0 || written >= AM_KMS_CARD_CAPACITY) {
            continue;
        }
        ++*count;
    }
    closedir(directory);
    for (size_t index = 0; index < *count; ++index) {
        for (size_t next = index + 1; next < *count; ++next) {
            if (strcmp(paths[index], paths[next]) > 0) {
                char temporary[AM_KMS_CARD_CAPACITY];
                memcpy(temporary, paths[index], sizeof(temporary));
                memcpy(paths[index], paths[next], sizeof(temporary));
                memcpy(paths[next], temporary, sizeof(temporary));
            }
        }
    }
    return *count > 0 ? AM_KMS_OK : AM_KMS_NO_DEVICE;
}

static int scan_outputs(struct am_kms_output outputs[AM_KMS_MAX_OUTPUTS],
                        size_t *count) {
    if (outputs == NULL || count == NULL) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    *count = 0;
    memset(outputs, 0, sizeof(*outputs) * AM_KMS_MAX_OUTPUTS);
    char paths[64][AM_KMS_CARD_CAPACITY] = {{0}};
    size_t path_count = 0;
    int result = collect_card_paths(paths, &path_count);
    if (result != AM_KMS_OK) {
        return result;
    }
    int first_error = AM_KMS_NO_OUTPUT;
    for (size_t index = 0; index < path_count; ++index) {
        result = scan_card(paths[index], outputs, count);
        if (result == AM_KMS_TOO_MANY_OUTPUTS) {
            return result;
        }
        if (result != AM_KMS_OK && first_error == AM_KMS_NO_OUTPUT) {
            first_error = result;
        }
    }
    if (*count == 0) {
        return first_error;
    }
    qsort(outputs, *count, sizeof(*outputs), compare_outputs);
    return AM_KMS_OK;
}

int am_kms_probe(struct am_kms_info *output) {
    if (output == NULL) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    memset(output, 0, sizeof(*output));
    struct am_kms_output outputs[AM_KMS_MAX_OUTPUTS];
    size_t count = 0;
    int result = scan_outputs(outputs, &count);
    output->output_count = (uint32_t)count;
    output->framebuffer_export_supported = 0;
    for (size_t index = 0; index < count; ++index) {
        output->framebuffer_export_supported |= outputs[index].framebuffer_export_supported;
    }
    output->available = output->framebuffer_export_supported != 0 ? 1U : 0U;
    if (output->available != 0) {
        output->error = AM_KMS_OK;
        return AM_KMS_OK;
    }
    output->error = (uint32_t)(result == AM_KMS_OK ? AM_KMS_UNSUPPORTED : result);
    return (int)output->error;
}

int am_kms_enumerate(struct am_kms_output *outputs, size_t capacity,
                     size_t *count) {
    if (count == NULL || (capacity > 0 && outputs == NULL)) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    *count = 0;
    struct am_kms_output discovered[AM_KMS_MAX_OUTPUTS];
    size_t discovered_count = 0;
    int result = scan_outputs(discovered, &discovered_count);
    if (result != AM_KMS_OK) {
        return result;
    }
    if (discovered_count > capacity) {
        *count = discovered_count;
        return AM_KMS_TOO_MANY_OUTPUTS;
    }
    memcpy(outputs, discovered, discovered_count * sizeof(*outputs));
    *count = discovered_count;
    return AM_KMS_OK;
}

static bool has_extension(const char *extensions, const char *required) {
    if (extensions == NULL || required == NULL) {
        return false;
    }
    size_t length = strlen(required);
    const char *cursor = extensions;
    while ((cursor = strstr(cursor, required)) != NULL) {
        bool starts = cursor == extensions || cursor[-1] == ' ';
        bool ends = cursor[length] == '\0' || cursor[length] == ' ';
        if (starts && ends) {
            return true;
        }
        cursor += length;
    }
    return false;
}

static int initialize_egl(struct am_kms_session *session) {
    session->gbm = gbm_create_device(session->card.fd);
    if (session->gbm == NULL) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (get_platform_display == NULL) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    session->display = get_platform_display(EGL_PLATFORM_GBM_KHR, session->gbm, NULL);
    if (session->display == EGL_NO_DISPLAY) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    EGLint major = 0;
    EGLint minor = 0;
    if (eglInitialize(session->display, &major, &minor) != EGL_TRUE) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    session->egl_initialized = true;
    if (eglBindAPI(EGL_OPENGL_ES_API) != EGL_TRUE) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    const char *egl_extensions = eglQueryString(session->display, EGL_EXTENSIONS);
    if (!has_extension(egl_extensions, "EGL_EXT_image_dma_buf_import")) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    session->create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    session->destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    session->image_to_texture =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (session->create_image == NULL || session->destroy_image == NULL ||
        session->image_to_texture == NULL) {
        return AM_KMS_EGL_UNAVAILABLE;
    }

    const EGLint config_attributes[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8,
        EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8,
        EGL_ALPHA_SIZE, 8,
        EGL_NONE,
    };
    EGLConfig config = NULL;
    EGLint config_count = 0;
    if (eglChooseConfig(session->display, config_attributes, &config, 1,
                        &config_count) != EGL_TRUE || config_count == 0) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    const EGLint surface_attributes[] = {EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE};
    session->surface = eglCreatePbufferSurface(session->display, config, surface_attributes);
    const EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    session->context = eglCreateContext(session->display, config, EGL_NO_CONTEXT,
                                        context_attributes);
    if (session->surface == EGL_NO_SURFACE || session->context == EGL_NO_CONTEXT ||
        eglMakeCurrent(session->display, session->surface, session->surface,
                       session->context) != EGL_TRUE) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    const char *gl_extensions = (const char *)glGetString(GL_EXTENSIONS);
    if (!has_extension(gl_extensions, "GL_OES_EGL_image")) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    return AM_KMS_OK;
}

static void clear_imported_image(struct am_kms_session *session) {
    if (session == NULL) {
        return;
    }
    bool gl_context_ready = session->display != EGL_NO_DISPLAY &&
                            session->context != EGL_NO_CONTEXT &&
                            session->surface != EGL_NO_SURFACE &&
                            eglMakeCurrent(session->display, session->surface,
                                           session->surface, session->context) == EGL_TRUE;
    /* A failed bind leaves the previous context current. Never apply this
     * session's GL object names to that unrelated context; destroying this
     * session's EGL context below releases its remaining GL objects instead. */
    if (gl_context_ready && session->framebuffer_object != 0) {
        glDeleteFramebuffers(1, &session->framebuffer_object);
    }
    session->framebuffer_object = 0;
    if (gl_context_ready && session->texture != 0) {
        glDeleteTextures(1, &session->texture);
    }
    session->texture = 0;
    if (session->image != EGL_NO_IMAGE_KHR && session->destroy_image != NULL) {
        (void)session->destroy_image(session->display, session->image);
        session->image = EGL_NO_IMAGE_KHR;
    }
    close_framebuffer(&session->framebuffer);
    memset(&session->framebuffer, 0, sizeof(session->framebuffer));
    session->framebuffer.dma_buf_fd = -1;
    session->framebuffer_id = 0;
}

static int import_framebuffer(struct am_kms_session *session,
                              struct am_kms_framebuffer *framebuffer) {
    if (session == NULL || framebuffer == NULL || framebuffer->dma_buf_fd < 0 ||
        framebuffer->width != session->output.width ||
        framebuffer->height != session->output.height ||
        !has_rgb32_format(framebuffer->fourcc)) {
        return AM_KMS_UNSUPPORTED;
    }
    const char *egl_extensions = eglQueryString(session->display, EGL_EXTENSIONS);
    const bool explicit_modifier = framebuffer->modifier != AM_KMS_INVALID_MODIFIER;
    if (explicit_modifier &&
        !has_extension(egl_extensions, "EGL_EXT_image_dma_buf_import_modifiers")) {
        return AM_KMS_UNSUPPORTED;
    }
    EGLint attributes[20];
    size_t index = 0;
    attributes[index++] = EGL_WIDTH;
    attributes[index++] = (EGLint)framebuffer->width;
    attributes[index++] = EGL_HEIGHT;
    attributes[index++] = (EGLint)framebuffer->height;
    attributes[index++] = EGL_LINUX_DRM_FOURCC_EXT;
    attributes[index++] = (EGLint)framebuffer->fourcc;
    attributes[index++] = EGL_DMA_BUF_PLANE0_FD_EXT;
    attributes[index++] = framebuffer->dma_buf_fd;
    attributes[index++] = EGL_DMA_BUF_PLANE0_OFFSET_EXT;
    attributes[index++] = (EGLint)framebuffer->offset;
    attributes[index++] = EGL_DMA_BUF_PLANE0_PITCH_EXT;
    attributes[index++] = (EGLint)framebuffer->pitch;
    if (explicit_modifier) {
        attributes[index++] = EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT;
        attributes[index++] = (EGLint)(uint32_t)(framebuffer->modifier & UINT32_MAX);
        attributes[index++] = EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT;
        attributes[index++] = (EGLint)(uint32_t)(framebuffer->modifier >> 32);
    }
    attributes[index++] = EGL_NONE;

    EGLImageKHR image = session->create_image(session->display, EGL_NO_CONTEXT,
                                               EGL_LINUX_DMA_BUF_EXT, NULL,
                                               attributes);
    if (image == EGL_NO_IMAGE_KHR) {
        return AM_KMS_UNSUPPORTED;
    }
    GLuint texture = 0;
    GLuint framebuffer_object = 0;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    session->image_to_texture(GL_TEXTURE_2D, (GLeglImageOES)image);
    glGenFramebuffers(1, &framebuffer_object);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer_object);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                           texture, 0);
    GLenum framebuffer_status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    GLenum gl_error = glGetError();
    if (texture == 0 || framebuffer_object == 0 ||
        framebuffer_status != GL_FRAMEBUFFER_COMPLETE || gl_error != GL_NO_ERROR) {
        if (framebuffer_object != 0) {
            glDeleteFramebuffers(1, &framebuffer_object);
        }
        if (texture != 0) {
            glDeleteTextures(1, &texture);
        }
        (void)session->destroy_image(session->display, image);
        return AM_KMS_UNSUPPORTED;
    }
    session->image = image;
    session->texture = texture;
    session->framebuffer_object = framebuffer_object;
    session->framebuffer = *framebuffer;
    framebuffer->dma_buf_fd = -1;
    session->framebuffer_id = session->framebuffer.id;
    return AM_KMS_OK;
}

static int refresh_image(struct am_kms_session *session) {
    drmModePlanePtr plane = drmModeGetPlane(session->card.fd, session->plane_id);
    if (plane == NULL || plane->fb_id == 0 || plane->crtc_id != session->crtc_id) {
        drmModeFreePlane(plane);
        return AM_KMS_OUTPUT_LOST;
    }
    uint32_t connector_id = 0;
    char name[AM_KMS_NAME_CAPACITY] = {0};
    drmModeResPtr resources = drmModeGetResources(session->card.fd);
    bool connector_live = connector_matches_crtc(session->card.fd, resources,
                                                  session->crtc_id,
                                                  &connector_id, name) &&
                          connector_id == session->connector_id;
    drmModeFreeResources(resources);
    drmModeCrtcPtr crtc = drmModeGetCrtc(session->card.fd, session->crtc_id);
    bool mode_live = crtc != NULL && crtc->mode_valid != 0 &&
                     crtc->width == session->output.width &&
                     crtc->height == session->output.height;
    if (!connector_live || !mode_live) {
        drmModeFreeCrtc(crtc);
        drmModeFreePlane(plane);
        return AM_KMS_OUTPUT_LOST;
    }
    if (!plane_geometry_supported(session->card.fd, session->plane_id,
                                  &session->framebuffer, crtc)) {
        drmModeFreeCrtc(crtc);
        drmModeFreePlane(plane);
        return AM_KMS_UNSUPPORTED;
    }
    uint32_t next_framebuffer_id = plane->fb_id;
    drmModeFreePlane(plane);
    if (session->framebuffer_id == next_framebuffer_id && session->image != EGL_NO_IMAGE_KHR) {
        drmModeFreeCrtc(crtc);
        return AM_KMS_OK;
    }

    struct am_kms_framebuffer next_framebuffer;
    int result = export_framebuffer(session->card.fd, next_framebuffer_id,
                                    &next_framebuffer);
    if (result != AM_KMS_OK) {
        drmModeFreeCrtc(crtc);
        return result;
    }
    if (!plane_geometry_supported(session->card.fd, session->plane_id,
                                  &next_framebuffer, crtc)) {
        close_framebuffer(&next_framebuffer);
        drmModeFreeCrtc(crtc);
        return AM_KMS_UNSUPPORTED;
    }
    drmModeFreeCrtc(crtc);
    clear_imported_image(session);
    result = import_framebuffer(session, &next_framebuffer);
    close_framebuffer(&next_framebuffer);
    return result;
}

static void destroy_egl(struct am_kms_session *session) {
    if (session == NULL) {
        return;
    }
    clear_imported_image(session);
    if (session->display != EGL_NO_DISPLAY && session->context != EGL_NO_CONTEXT) {
        (void)eglMakeCurrent(session->display, EGL_NO_SURFACE, EGL_NO_SURFACE,
                             EGL_NO_CONTEXT);
    }
    if (session->display != EGL_NO_DISPLAY && session->surface != EGL_NO_SURFACE) {
        (void)eglDestroySurface(session->display, session->surface);
        session->surface = EGL_NO_SURFACE;
    }
    if (session->display != EGL_NO_DISPLAY && session->context != EGL_NO_CONTEXT) {
        (void)eglDestroyContext(session->display, session->context);
        session->context = EGL_NO_CONTEXT;
    }
    if (session->display != EGL_NO_DISPLAY && session->egl_initialized) {
        (void)eglTerminate(session->display);
        session->egl_initialized = false;
    }
    session->display = EGL_NO_DISPLAY;
    if (session->gbm != NULL) {
        gbm_device_destroy(session->gbm);
        session->gbm = NULL;
    }
}

struct am_kms_session *am_kms_open(const char *output_id, uint32_t *error) {
    if (error == NULL || output_id == NULL) {
        if (error != NULL) {
            *error = AM_KMS_INVALID_ARGUMENT;
        }
        return NULL;
    }
    *error = AM_KMS_OK;
    char card_name[AM_KMS_CARD_CAPACITY] = {0};
    uint32_t connector_id = 0;
    uint32_t crtc_id = 0;
    uint32_t plane_id = 0;
    int result = parse_output_id(output_id, card_name, &connector_id, &crtc_id,
                                 &plane_id);
    if (result != AM_KMS_OK) {
        *error = (uint32_t)result;
        return NULL;
    }
    char path[AM_KMS_CARD_CAPACITY];
    int path_length = snprintf(path, sizeof(path), "/dev/dri/%s", card_name);
    if (path_length <= 0 || (size_t)path_length >= sizeof(path)) {
        *error = AM_KMS_INVALID_ARGUMENT;
        return NULL;
    }

    struct am_kms_output outputs[AM_KMS_MAX_OUTPUTS];
    size_t count = 0;
    result = scan_outputs(outputs, &count);
    if (result != AM_KMS_OK) {
        *error = (uint32_t)result;
        return NULL;
    }
    struct am_kms_output selected;
    bool found = false;
    for (size_t index = 0; index < count; ++index) {
        if (strcmp(outputs[index].id, output_id) == 0) {
            selected = outputs[index];
            found = true;
            break;
        }
    }
    if (!found) {
        *error = AM_KMS_OUTPUT_LOST;
        return NULL;
    }
    if (selected.framebuffer_export_supported == 0) {
        *error = selected.error == AM_KMS_OK ? AM_KMS_UNSUPPORTED : selected.error;
        return NULL;
    }

    struct am_kms_session *session = calloc(1, sizeof(*session));
    if (session == NULL) {
        *error = AM_KMS_CAPTURE_FAILED;
        return NULL;
    }
    session->card.fd = -1;
    session->framebuffer.dma_buf_fd = -1;
    session->display = EGL_NO_DISPLAY;
    session->context = EGL_NO_CONTEXT;
    session->surface = EGL_NO_SURFACE;
    session->image = EGL_NO_IMAGE_KHR;
    memcpy(session->output.id, selected.id, sizeof(session->output.id));
    memcpy(session->output.name, selected.name, sizeof(session->output.name));
    memcpy(session->output.card, selected.card, sizeof(session->output.card));
    session->output.left = selected.left;
    session->output.top = selected.top;
    session->output.width = selected.width;
    session->output.height = selected.height;
    session->output.rotation = selected.rotation;
    session->output.attached = selected.attached;
    session->output.framebuffer_export_supported = selected.framebuffer_export_supported;
    session->connector_id = connector_id;
    session->crtc_id = crtc_id;
    session->plane_id = plane_id;

    result = open_card(path, &session->card);
    if (result == AM_KMS_OK) {
        result = initialize_egl(session);
    }
    if (result == AM_KMS_OK) {
        result = refresh_image(session);
    }
    if (result != AM_KMS_OK) {
        *error = (uint32_t)result;
        am_kms_close(session);
        return NULL;
    }
    return session;
}

int am_kms_session_geometry(struct am_kms_session *session,
                            struct am_kms_output *output) {
    if (session == NULL || output == NULL) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    *output = session->output;
    return AM_KMS_OK;
}

int am_kms_read(struct am_kms_session *session, uint8_t *destination,
                size_t length, uint32_t *width, uint32_t *height) {
    if (session == NULL || destination == NULL || width == NULL || height == NULL) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    *width = 0;
    *height = 0;
    uint64_t expected_length = (uint64_t)session->output.width *
                               session->output.height * 4U;
    if (session->output.width == 0 || session->output.height == 0 ||
        session->output.width > AM_KMS_MAX_DIMENSION ||
        session->output.height > AM_KMS_MAX_DIMENSION ||
        expected_length > AM_KMS_MAX_FRAME_BYTES || length != expected_length) {
        return AM_KMS_INVALID_ARGUMENT;
    }
    if (eglMakeCurrent(session->display, session->surface, session->surface,
                       session->context) != EGL_TRUE) {
        return AM_KMS_EGL_UNAVAILABLE;
    }
    int result = refresh_image(session);
    if (result != AM_KMS_OK) {
        return result;
    }
    size_t pixel_count = (size_t)session->output.width * session->output.height;
    uint8_t *rgba = malloc(pixel_count * 4U);
    if (rgba == NULL) {
        return AM_KMS_CAPTURE_FAILED;
    }
    glBindFramebuffer(GL_FRAMEBUFFER, session->framebuffer_object);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glFinish();
    glReadPixels(0, 0, (GLsizei)session->output.width,
                 (GLsizei)session->output.height, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    GLenum gl_error = glGetError();
    if (gl_error != GL_NO_ERROR) {
        free(rgba);
        return AM_KMS_CAPTURE_FAILED;
    }
    const size_t row_bytes = (size_t)session->output.width * 4U;
    for (uint32_t y = 0; y < session->output.height; ++y) {
        const uint8_t *source = rgba +
            (size_t)(session->output.height - y - 1U) * row_bytes;
        uint8_t *target = destination + (size_t)y * row_bytes;
        for (uint32_t x = 0; x < session->output.width; ++x) {
            target[x * 4U + 0U] = source[x * 4U + 2U];
            target[x * 4U + 1U] = source[x * 4U + 1U];
            target[x * 4U + 2U] = source[x * 4U + 0U];
            target[x * 4U + 3U] = 255U;
        }
    }
    free(rgba);
    *width = session->output.width;
    *height = session->output.height;
    return AM_KMS_OK;
}

void am_kms_close(struct am_kms_session *session) {
    if (session == NULL) {
        return;
    }
    destroy_egl(session);
    close_card(&session->card);
    free(session);
}
