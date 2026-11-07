#ifndef AMATERASU_KWIN_CAPTURE_H
#define AMATERASU_KWIN_CAPTURE_H
#include <stdint.h>
#include <stdatomic.h>
#include <stdbool.h>
struct am_kwin;
struct am_kwin_geometry { int32_t left,top; uint32_t width,height; int32_t desktop_left,desktop_top; uint32_t desktop_width,desktop_height; uint32_t registry_global,mode_width,mode_height,refresh_millihz; char output_name[128],compositor_id[65]; };
/* The peer process identity is scoped to the caller's PID namespace. */
int am_kwin_peer_identity(int fd, char identity[65]);
int am_kwin_validate_peer_identity(int fd, const char *pinned, char identity[65]);
int am_kwin_compositor_digest(const char *boot_id, uint32_t pid, uint64_t starttime, char identity[65]);
/* Connection, Wayland proxies and event callbacks remain on one native worker.
 * The borrowed cancellation flag outlives open/poll and all callbacks. */
struct am_kwin *am_kwin_open(const char *output,const atomic_bool *stopped,int64_t deadline_us);
/* Read-only bounded registry/output inspection; never creates a screencast. */
int am_kwin_preflight(const char *output,int managed,uint32_t width,uint32_t height,uint32_t refresh_millihz);
int am_kwin_poll(struct am_kwin *connection);
int am_kwin_snapshot(struct am_kwin *connection,uint32_t *node,uint64_t *serial,struct am_kwin_geometry *geometry);
void am_kwin_close(struct am_kwin *connection);
#endif
