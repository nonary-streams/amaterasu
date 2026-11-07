#include "dispatch.h"
#include <stdbool.h>
#include <stdint.h>

struct dispatch_fixture {
    atomic_bool stopped;
    unsigned calls;
    bool stop_after_first;
};

static gboolean dispatch_ready(gpointer data) {
    struct dispatch_fixture *fixture = data;
    fixture->calls++;
    if (fixture->stop_after_first) atomic_store(&fixture->stopped, true);
    /* A broken dispatcher still terminates deterministically, without a timer
     * or another thread, so this regression cannot hang the test process. */
    return fixture->calls < 4 ? G_SOURCE_CONTINUE : G_SOURCE_REMOVE;
}

uint32_t am_pw_test_dispatch_pending(uint32_t stop_mode) {
    struct dispatch_fixture fixture = { .calls = 0, .stop_after_first = stop_mode == 1 };
    atomic_init(&fixture.stopped, stop_mode == 2);
    GMainContext *context = g_main_context_new();
    GSource *source = g_idle_source_new();
    g_source_set_callback(source, dispatch_ready, &fixture, NULL);
    g_source_attach(source, context);

    am_pw_dispatch_pending(context, &fixture.stopped);

    g_source_destroy(source);
    g_source_unref(source);
    g_main_context_unref(context);
    return fixture.calls;
}
