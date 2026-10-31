#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <wayland-client.h>

#define LIMIT 64
struct observer;
struct entry {
  struct observer *owner;
  struct wl_output *output;
  uint32_t global;
  char name[128];
  int32_t width, height, refresh;
  uint64_t generation;
  bool alive;
};
struct observer {
  struct wl_display *display;
  struct wl_registry *registry;
  struct entry entries[LIMIT];
  bool failed;
  uint64_t generation;
};
struct observation {
  uint32_t global, width, height, refresh;
  uint64_t generation;
};
static void geometry(void *d, struct wl_output *o, int32_t x, int32_t y,
                     int32_t pw, int32_t ph, int32_t sub, const char *make,
                     const char *model, int32_t transform) {
  (void)d;
  (void)o;
  (void)x;
  (void)y;
  (void)pw;
  (void)ph;
  (void)sub;
  (void)make;
  (void)model;
  (void)transform;
}
static void mode(void *d, struct wl_output *o, uint32_t flags, int32_t w,
                 int32_t h, int32_t r) {
  (void)o;
  struct entry *e = d;
  if (flags & WL_OUTPUT_MODE_CURRENT) {
    if (e->width != w || e->height != h || e->refresh != r)
      e->generation = ++e->owner->generation;
    e->width = w;
    e->height = h;
    e->refresh = r;
  }
}
static void done(void *d, struct wl_output *o) {
  (void)d;
  (void)o;
}
static void scale(void *d, struct wl_output *o, int32_t s) {
  (void)d;
  (void)o;
  (void)s;
}
static void name(void *d, struct wl_output *o, const char *n) {
  (void)o;
  struct entry *e = d;
  if (!n || strlen(n) >= sizeof(e->name)) {
    e->alive = false;
    return;
  }
  if (strcmp(e->name, n) != 0)
    e->generation = ++e->owner->generation;
  strcpy(e->name, n);
}
static void description(void *d, struct wl_output *o, const char *n) {
  (void)d;
  (void)o;
  (void)n;
}
static const struct wl_output_listener output_listener = {
    geometry, mode, done, scale, name, description};
static void global(void *d, struct wl_registry *r, uint32_t id,
                   const char *interface, uint32_t version) {
  struct observer *s = d;
  if (strcmp(interface, "wl_output") != 0)
    return;
  if (version < 4) {
    s->failed = true;
    return;
  }
  size_t i;
  for (i = 0; i < LIMIT; i++)
    if (!s->entries[i].output)
      break;
  if (i == LIMIT) {
    s->failed = true;
    return;
  }
  struct entry *e = &s->entries[i];
  e->owner = s;
  e->global = id;
  e->alive = true;
  e->generation = ++s->generation;
  e->output = wl_registry_bind(r, id, &wl_output_interface, 4);
  if (!e->output || wl_output_add_listener(e->output, &output_listener, e) < 0)
    s->failed = true;
}
static void removed(void *d, struct wl_registry *r, uint32_t id) {
  (void)r;
  struct observer *s = d;
  for (size_t i = 0; i < LIMIT; i++)
    if (s->entries[i].output && s->entries[i].global == id) {
      s->entries[i].alive = false;
      s->entries[i].generation = ++s->generation;
    }
}
static const struct wl_registry_listener registry_listener = {global, removed};
static void synced(void *d, struct wl_callback *cb, uint32_t serial) {
  (void)serial;
  *(bool *)d = true;
  wl_callback_destroy(cb);
}
static const struct wl_callback_listener sync_listener = {synced};
static int sync_display(struct observer *s) {
  bool complete = false;
  struct wl_callback *cb = wl_display_sync(s->display);
  if (!cb)
    return -1;
  wl_callback_add_listener(cb, &sync_listener, &complete);
  struct timespec start, now;
  if (clock_gettime(CLOCK_MONOTONIC, &start) < 0) {
    wl_callback_destroy(cb);
    return -1;
  }
  while (!complete) {
    if (wl_display_dispatch_pending(s->display) < 0 ||
        wl_display_flush(s->display) < 0) {
      wl_callback_destroy(cb);
      return -1;
    }
    if (complete)
      break;
    if (clock_gettime(CLOCK_MONOTONIC, &now) < 0) {
      wl_callback_destroy(cb);
      return -1;
    }
    long ms = 1000 - (now.tv_sec - start.tv_sec) * 1000 -
              (now.tv_nsec - start.tv_nsec) / 1000000;
    if (ms <= 0) {
      wl_callback_destroy(cb);
      return -1;
    }
    struct pollfd fd = {wl_display_get_fd(s->display), POLLIN, 0};
    int n;
    do {
      n = poll(&fd, 1, (int)ms);
    } while (n < 0 && errno == EINTR);
    if (n <= 0 || !(fd.revents & POLLIN) ||
        wl_display_dispatch(s->display) < 0) {
      wl_callback_destroy(cb);
      return -1;
    }
  }
  return s->failed ? -1 : 0;
}
void *am_display_observer_open(void) {
  struct observer *s = calloc(1, sizeof(*s));
  if (!s)
    return NULL;
  s->display = wl_display_connect(NULL);
  if (!s->display) {
    free(s);
    return NULL;
  }
  s->registry = wl_display_get_registry(s->display);
  if (!s->registry ||
      wl_registry_add_listener(s->registry, &registry_listener, s) < 0 ||
      sync_display(s) < 0 || sync_display(s) < 0) {
    if (s->registry)
      wl_registry_destroy(s->registry);
    wl_display_disconnect(s->display);
    free(s);
    return NULL;
  }
  return s;
}
void am_display_observer_close(void *p) {
  struct observer *s = p;
  if (!s)
    return;
  for (size_t i = 0; i < LIMIT; i++)
    if (s->entries[i].output)
      wl_output_release(s->entries[i].output);
  wl_registry_destroy(s->registry);
  wl_display_disconnect(s->display);
  free(s);
}
int am_display_observer_read(void *p, const char *wanted,
                             struct observation *out) {
  struct observer *s = p;
  if (!s || !wanted || !out || sync_display(s) < 0) {
    if (s)
      s->failed = true;
    return -1;
  }
  struct entry *found = NULL;
  for (size_t i = 0; i < LIMIT; i++) {
    struct entry *e = &s->entries[i];
    if (e->alive && strcmp(e->name, wanted) == 0) {
      if (found)
        return -1;
      found = e;
    }
  }
  if (!found || found->width <= 0 || found->height <= 0 || found->refresh <= 0)
    return -1;
  *out = (struct observation){found->global, (uint32_t)found->width,
                              (uint32_t)found->height, (uint32_t)found->refresh,
                              found->generation};
  return 0;
}
