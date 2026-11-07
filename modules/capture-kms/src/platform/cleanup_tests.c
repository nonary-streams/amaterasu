/* This object is pulled from the adapter archive only by Rust unit tests.
 * All exercised EGL/GL/DRM calls are mocks; no display or device is opened. */
#define eglMakeCurrent am_kms_test_make_current
#define glDeleteTextures am_kms_test_delete_textures
#define glDeleteFramebuffers am_kms_test_delete_framebuffers
#define drmModeGetPlane am_kms_test_get_plane
#define am_kms_probe am_kms_test_probe
#define am_kms_enumerate am_kms_test_enumerate
#define am_kms_open am_kms_test_open
#define am_kms_session_geometry am_kms_test_session_geometry
#define am_kms_read am_kms_test_read
#define am_kms_close am_kms_test_close
#include "adapter.c"
#undef eglMakeCurrent
#undef glDeleteTextures
#undef glDeleteFramebuffers
#undef drmModeGetPlane

struct am_kms_cleanup_observation {
    uint32_t make_current_calls;
    uint32_t texture_deletions;
    uint32_t framebuffer_deletions;
    uint32_t wrong_context_deletions;
    uint32_t image_deletions;
    uint32_t plane_queries;
    uint32_t texture_after;
    uint32_t framebuffer_after;
};

struct cleanup_trace {
    struct am_kms_cleanup_observation observed;
    EGLBoolean make_current_succeeds;
    bool owns_current_context;
};

static _Thread_local struct cleanup_trace trace;

EGLBoolean EGLAPIENTRY am_kms_test_make_current(EGLDisplay display,
    EGLSurface draw, EGLSurface read, EGLContext context) {
    (void)display; (void)draw; (void)read; (void)context;
    trace.observed.make_current_calls++;
    if (trace.make_current_succeeds == EGL_TRUE) {
        trace.owns_current_context = true;
    }
    return trace.make_current_succeeds;
}

void GL_APIENTRY am_kms_test_delete_textures(GLsizei count, const GLuint *textures) {
    (void)textures;
    trace.observed.texture_deletions += (uint32_t)count;
    if (!trace.owns_current_context) trace.observed.wrong_context_deletions++;
}

void GL_APIENTRY am_kms_test_delete_framebuffers(GLsizei count,
    const GLuint *framebuffers) {
    (void)framebuffers;
    trace.observed.framebuffer_deletions += (uint32_t)count;
    if (!trace.owns_current_context) trace.observed.wrong_context_deletions++;
}

drmModePlanePtr am_kms_test_get_plane(int fd, uint32_t plane) {
    (void)fd; (void)plane;
    trace.observed.plane_queries++;
    return NULL;
}

static EGLBoolean EGLAPIENTRY destroy_test_image(EGLDisplay display,
    EGLImageKHR image) {
    (void)display; (void)image;
    trace.observed.image_deletions++;
    return EGL_TRUE;
}

static struct am_kms_session test_session(void) {
    struct am_kms_session session = {0};
    /* Opaque mock-only handles are never passed to a native implementation. */
    session.display = (EGLDisplay)(uintptr_t)1;
    session.context = (EGLContext)(uintptr_t)2;
    session.surface = (EGLSurface)(uintptr_t)3;
    session.image = (EGLImageKHR)(uintptr_t)4;
    session.texture = 11;
    session.framebuffer_object = 12;
    session.framebuffer.dma_buf_fd = -1;
    session.card.fd = -1;
    session.destroy_image = destroy_test_image;
    session.output.width = 2;
    session.output.height = 2;
    return session;
}

void am_kms_test_image_cleanup(uint32_t make_current_succeeds,
    uint32_t has_context, struct am_kms_cleanup_observation *output) {
    trace = (struct cleanup_trace){0};
    trace.make_current_succeeds = make_current_succeeds ? EGL_TRUE : EGL_FALSE;
    struct am_kms_session session = test_session();
    if (!has_context) session.context = EGL_NO_CONTEXT;
    clear_imported_image(&session);
    trace.observed.texture_after = session.texture;
    trace.observed.framebuffer_after = session.framebuffer_object;
    *output = trace.observed;
}

int am_kms_test_read_without_current_context(
    struct am_kms_cleanup_observation *output) {
    trace = (struct cleanup_trace){0};
    struct am_kms_session session = test_session();
    uint8_t pixels[16] = {0};
    uint32_t width = 0, height = 0;
    int result = am_kms_test_read(&session, pixels, sizeof(pixels), &width, &height);
    *output = trace.observed;
    return result;
}
