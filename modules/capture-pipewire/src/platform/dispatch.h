#ifndef AMATERASU_PIPEWIRE_DISPATCH_H
#define AMATERASU_PIPEWIRE_DISPATCH_H

#include <glib.h>
#include <stdatomic.h>

static inline void am_pw_dispatch_pending(GMainContext *context,
                                          const atomic_bool *stopped) {
    /* An always-ready source must not keep a retiring worker in this drain. */
    while (!atomic_load(stopped) && g_main_context_iteration(context, FALSE)) {}
}

#endif
