/* This object is pulled from the adapter archive only by Rust unit tests.
 * All exercised EGL/GL/DRM calls are mocks; no display or device is opened. */
#define eglMakeCurrent am_kms_test_make_current
#define eglGetCurrentContext am_kms_test_get_current_context
#define eglGetCurrentDisplay am_kms_test_get_current_display
#define eglQueryAPI am_kms_test_query_api
#define eglBindAPI am_kms_test_bind_api
#define eglDestroySurface am_kms_test_destroy_surface
#define eglDestroyContext am_kms_test_destroy_context
#define eglTerminate am_kms_test_terminate
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
#undef eglGetCurrentContext
#undef eglGetCurrentDisplay
#undef eglQueryAPI
#undef eglBindAPI
#undef eglDestroySurface
#undef eglDestroyContext
#undef eglTerminate
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
    uint32_t unbind_calls;
    uint32_t foreign_context_unbinds;
    uint32_t surface_destructions;
    uint32_t context_destructions;
    uint32_t display_terminations;
    uint32_t current_context_after;
    uint32_t selected_api_after;
    uint32_t vg_context_after;
    uint32_t foreign_display_terminations;
    uint32_t api_bind_calls;
};

struct cleanup_trace {
    struct am_kms_cleanup_observation observed;
    EGLBoolean make_current_succeeds;
    bool owns_current_context;
    EGLContext current_context;
    EGLDisplay current_display;
    EGLenum selected_api;
    EGLContext vg_context;
    EGLDisplay vg_display;
    uint32_t fail_bind_call;
};

static _Thread_local struct cleanup_trace trace;

EGLBoolean EGLAPIENTRY am_kms_test_make_current(EGLDisplay display,
    EGLSurface draw, EGLSurface read, EGLContext context) {
    (void)draw; (void)read;
    trace.observed.make_current_calls++;
    if (context == EGL_NO_CONTEXT) {
        trace.observed.unbind_calls++;
        if (trace.selected_api == EGL_OPENVG_API) {
            if (trace.vg_context != EGL_NO_CONTEXT) {
                trace.observed.foreign_context_unbinds++;
            }
            trace.vg_context = EGL_NO_CONTEXT;
            trace.vg_display = EGL_NO_DISPLAY;
            return EGL_TRUE;
        }
        if (trace.current_context != EGL_NO_CONTEXT &&
            trace.current_context != (EGLContext)(uintptr_t)2) {
            trace.observed.foreign_context_unbinds++;
        }
        trace.current_context = EGL_NO_CONTEXT;
        trace.current_display = EGL_NO_DISPLAY;
        trace.owns_current_context = false;
        return EGL_TRUE;
    }
    if (trace.make_current_succeeds == EGL_TRUE) {
        trace.owns_current_context = true;
        trace.current_context = context;
        trace.current_display = display;
    }
    return trace.make_current_succeeds;
}

EGLContext EGLAPIENTRY am_kms_test_get_current_context(void) {
    return trace.selected_api == EGL_OPENVG_API ? trace.vg_context : trace.current_context;
}

EGLDisplay EGLAPIENTRY am_kms_test_get_current_display(void) {
    return trace.selected_api == EGL_OPENVG_API ? trace.vg_display : trace.current_display;
}

EGLenum EGLAPIENTRY am_kms_test_query_api(void) {
    return trace.selected_api;
}

EGLBoolean EGLAPIENTRY am_kms_test_bind_api(EGLenum api) {
    trace.observed.api_bind_calls++;
    if (trace.observed.api_bind_calls == trace.fail_bind_call) return EGL_FALSE;
    trace.selected_api = api;
    return EGL_TRUE;
}

EGLBoolean EGLAPIENTRY am_kms_test_destroy_surface(EGLDisplay display, EGLSurface surface) {
    (void)display; (void)surface;
    trace.observed.surface_destructions++;
    return EGL_TRUE;
}

EGLBoolean EGLAPIENTRY am_kms_test_destroy_context(EGLDisplay display, EGLContext context) {
    (void)display; (void)context;
    trace.observed.context_destructions++;
    return EGL_TRUE;
}

EGLBoolean EGLAPIENTRY am_kms_test_terminate(EGLDisplay display) {
    trace.observed.display_terminations++;
    /* Real EGL marks every resource on this display for deletion, including
     * a foreign current context. Record that separately from explicit unbind. */
    if (trace.current_display == display &&
        trace.current_context != EGL_NO_CONTEXT &&
        trace.current_context != (EGLContext)(uintptr_t)2) {
        trace.observed.foreign_display_terminations++;
    }
    return EGL_TRUE;
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

void am_kms_test_api_cleanup(uint32_t scenario,
    struct am_kms_cleanup_observation *output) {
    trace = (struct cleanup_trace){0};
    trace.selected_api = scenario == 3 ? EGL_NONE : EGL_OPENVG_API;
    trace.current_display = (EGLDisplay)(uintptr_t)1;
    trace.current_context = (EGLContext)(uintptr_t)2;
    trace.vg_context = (EGLContext)(uintptr_t)6;
    trace.vg_display = (EGLDisplay)(uintptr_t)7;
    trace.fail_bind_call = scenario == 1 ? 1 : scenario == 2 ? 2 : 0;
    struct am_kms_session session = test_session();
    if (scenario == 0) session.context = EGL_NO_CONTEXT;
    destroy_egl(&session);
    trace.observed.current_context_after = (uint32_t)(uintptr_t)trace.current_context;
    trace.observed.selected_api_after = trace.selected_api;
    trace.observed.vg_context_after = (uint32_t)(uintptr_t)trace.vg_context;
    *output = trace.observed;
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

void am_kms_test_full_cleanup(uint32_t make_current_succeeds,
    uint32_t initially_owned, uint32_t selected_vg,
    struct am_kms_cleanup_observation *output) {
    trace = (struct cleanup_trace){0};
    trace.make_current_succeeds = make_current_succeeds ? EGL_TRUE : EGL_FALSE;
    trace.current_display = (EGLDisplay)(uintptr_t)1;
    trace.current_context = (EGLContext)(uintptr_t)(initially_owned ? 2 : 5);
    trace.owns_current_context = initially_owned != 0;
    trace.selected_api = selected_vg ? EGL_OPENVG_API : EGL_OPENGL_ES_API;
    trace.vg_context = (EGLContext)(uintptr_t)6;
    trace.vg_display = (EGLDisplay)(uintptr_t)7;
    struct am_kms_session session = test_session();
    session.egl_initialized = true;
    destroy_egl(&session);
    trace.observed.current_context_after = (uint32_t)(uintptr_t)trace.current_context;
    trace.observed.selected_api_after = trace.selected_api;
    trace.observed.vg_context_after = (uint32_t)(uintptr_t)trace.vg_context;
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
