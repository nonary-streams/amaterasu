#define _GNU_SOURCE
#include "kwin.h"
#include "zkde-screencast-unstable-v1.h"
#include "xdg-output-unstable-v1.h"
#include <wayland-client.h>
#include <glib.h>
#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

#define MAX_OUTPUTS 64
struct output {
    uint32_t global;
    struct wl_output *object;
    struct zxdg_output_v1 *xdg;
    char name[128];
    int32_t x,y,width,height;
    uint32_t mode_width,mode_height,refresh_millihz;
    int32_t pending_x,pending_y,pending_width,pending_height;
    uint32_t pending_mode_width,pending_mode_height,pending_refresh_millihz;
    bool geometry_pending,mode_pending;
    bool removed;
};
struct am_kwin {
    struct wl_display *display;
    struct wl_registry *registry;
    struct zkde_screencast_unstable_v1 *manager;
    struct zkde_screencast_stream_unstable_v1 *stream;
    struct zxdg_output_manager_v1 *xdg_manager;
    struct output outputs[MAX_OUTPUTS];
    size_t count,selected;
    uint32_t node;
    uint64_t serial;
    char compositor_id[65];
    bool failed;
};

int am_kwin_compositor_digest(const char *boot_id,uint32_t pid,uint64_t starttime,char identity[65]) {
    if (!boot_id || !identity || !pid || !starttime || strlen(boot_id)!=36) return -1;
    for (size_t i=0;i<36;i++) {
        bool hyphen=i==8 || i==13 || i==18 || i==23;
        if (hyphen ? boot_id[i]!='-' : !((boot_id[i]>='0' && boot_id[i]<='9') || (boot_id[i]>='a' && boot_id[i]<='f'))) return -1;
    }
    char input[160];
    int length=snprintf(input,sizeof(input),"amaterasu-wayland-compositor-v1\n%s\n%u\n%llu\n",boot_id,pid,(unsigned long long)starttime);
    if (length<0 || (size_t)length>=sizeof(input)) return -1;
    char *digest=g_compute_checksum_for_string(G_CHECKSUM_SHA256,input,length);
    if (!digest) return -1;
    memcpy(identity,digest,65); g_free(digest); return 0;
}

static int peer_credentials(int fd,struct ucred *peer) {
    socklen_t size=sizeof(*peer);
    return getsockopt(fd,SOL_SOCKET,SO_PEERCRED,peer,&size)==0 && size==sizeof(*peer) && peer->pid>0 ? 0:-1;
}

static int read_starttime(pid_t pid,uint64_t *starttime) {
    char path[64],line[8192];
    if (snprintf(path,sizeof(path),"/proc/%ld/stat",(long)pid)<0) return -1;
    FILE *file=fopen(path,"r"); if (!file) return -1;
    bool read_ok=fgets(line,sizeof(line),file)!=NULL && strchr(line,'\n')!=NULL;
    fclose(file); if (!read_ok) return -1;
    char *end=strrchr(line,')'); if (!end || end[1]!=' ') return -1;
    char *save=NULL, *field=strtok_r(end+2," ",&save);
    for (unsigned index=3;field && index<22;index++) field=strtok_r(NULL," ",&save);
    if (!field || !*field) return -1;
    for (char *p=field;*p;p++) if (*p<'0' || *p>'9') return -1;
    errno=0; char *last=NULL; unsigned long long value=strtoull(field,&last,10);
    if (errno || !last || *last || !value) return -1;
    *starttime=(uint64_t)value; return 0;
}

int am_kwin_peer_identity(int fd,char identity[65]) {
    struct ucred before,after; uint64_t first,second;
    char boot_id[40];
    if (peer_credentials(fd,&before)<0 || read_starttime(before.pid,&first)<0) return -1;
    FILE *file=fopen("/proc/sys/kernel/random/boot_id","r"); if (!file) return -1;
    bool read_ok=fgets(boot_id,sizeof(boot_id),file)!=NULL; fclose(file);
    if (!read_ok) return -1;
    size_t length=strlen(boot_id);
    if (length!=37 || boot_id[36]!='\n') return -1;
    boot_id[36]='\0';
    if (peer_credentials(fd,&after)<0 || before.pid!=after.pid || before.uid!=after.uid || before.gid!=after.gid ||
        read_starttime(after.pid,&second)<0 || first!=second) return -1;
    return am_kwin_compositor_digest(boot_id,(uint32_t)before.pid,first,identity);
}

int am_kwin_validate_peer_identity(int fd,const char *pinned,char identity[65]) {
    char current[65];
    identity[0]='\0';
    if (!pinned || strlen(pinned)!=64 || am_kwin_peer_identity(fd,current)<0 || strcmp(current,pinned)!=0) return -1;
    memcpy(identity,current,65); return 0;
}
static void output_geometry(void *data,struct wl_output *object,int32_t x,int32_t y,int32_t pw,int32_t ph,int32_t sub,const char *make,const char *model,int32_t transform) {
    (void)data;(void)object;(void)x;(void)y;(void)pw;(void)ph;(void)sub;(void)make;(void)model;(void)transform;
}
static void output_mode(void *data,struct wl_output *object,uint32_t flags,int32_t width,int32_t height,int32_t refresh) {
    (void)object; struct output *output=data;
    if (flags & WL_OUTPUT_MODE_CURRENT) {
        output->pending_mode_width=width>0?(uint32_t)width:0; output->pending_mode_height=height>0?(uint32_t)height:0;
        output->pending_refresh_millihz=refresh>0?(uint32_t)refresh:0; output->mode_pending=true;
    }
}
static void commit_geometry(struct output *output) {
    if (!output->geometry_pending) return;
    output->x=output->pending_x; output->y=output->pending_y;
    output->width=output->pending_width; output->height=output->pending_height;
    output->geometry_pending=false;
}
static void output_done(void *data,struct wl_output *object) {
    (void)object; struct output *output=data;
    if (output->mode_pending) {
        output->mode_width=output->pending_mode_width; output->mode_height=output->pending_mode_height;
        output->refresh_millihz=output->pending_refresh_millihz; output->mode_pending=false;
    }
    if (output->xdg && zxdg_output_v1_get_version(output->xdg)>=3) commit_geometry(output);
}
static void output_scale(void *data,struct wl_output *object,int32_t scale) { (void)data;(void)object;(void)scale; }
static void output_name(void *data,struct wl_output *object,const char *name) {
    (void)object; struct output *output=data;
    if (name && strlen(name)<sizeof(output->name)) memcpy(output->name,name,strlen(name)+1);
}
static void output_description(void *data,struct wl_output *object,const char *description) { (void)data;(void)object;(void)description; }
static const struct wl_output_listener output_listener={.geometry=output_geometry,.mode=output_mode,.done=output_done,.scale=output_scale,.name=output_name,.description=output_description};
static void begin_geometry(struct output *output) {
    if (output->geometry_pending) return;
    output->pending_x=output->x; output->pending_y=output->y;
    output->pending_width=output->width; output->pending_height=output->height;
    output->geometry_pending=true;
}
static void logical_position(void *data,struct zxdg_output_v1 *object,int32_t x,int32_t y) { (void)object;struct output *output=data;begin_geometry(output);output->pending_x=x;output->pending_y=y; }
static void logical_size(void *data,struct zxdg_output_v1 *object,int32_t width,int32_t height) { (void)object;struct output *output=data;begin_geometry(output);output->pending_width=width;output->pending_height=height; }
static void logical_done(void *data,struct zxdg_output_v1 *object) { (void)object;commit_geometry(data); }
static void logical_name(void *data,struct zxdg_output_v1 *object,const char *name) { (void)object;output_name(data,NULL,name); }
static void logical_description(void *data,struct zxdg_output_v1 *object,const char *description) { (void)data;(void)object;(void)description; }
static const struct zxdg_output_v1_listener logical_listener={.logical_position=logical_position,.logical_size=logical_size,.done=logical_done,.name=logical_name,.description=logical_description};
static void release_output(struct output *output) {
    if (output->xdg) zxdg_output_v1_destroy(output->xdg);
    if (output->object) {
        if (wl_output_get_version(output->object) >= 3) wl_output_release(output->object);
        else wl_output_destroy(output->object);
    }
    memset(output, 0, sizeof(*output));
}
static void global(void *data,struct wl_registry *registry,uint32_t name,const char *interface,uint32_t version) {
    struct am_kwin *connection=data;
    if (strcmp(interface,zkde_screencast_unstable_v1_interface.name)==0 && !connection->manager) {
        connection->manager=wl_registry_bind(registry,name,&zkde_screencast_unstable_v1_interface,version<6?version:6);
    } else if (strcmp(interface,zxdg_output_manager_v1_interface.name)==0 && !connection->xdg_manager) {
        connection->xdg_manager=wl_registry_bind(registry,name,&zxdg_output_manager_v1_interface,version<3?version:3);
    } else if (strcmp(interface,wl_output_interface.name)==0) {
        size_t index=0;
        while (index<connection->count && !connection->outputs[index].removed) ++index;
        if (index>=MAX_OUTPUTS || version<2) { connection->failed=true;return; }
        if (index==connection->count) ++connection->count;
        struct output *output=&connection->outputs[index];
        if (output->removed) release_output(output);
        output->global=name;
        output->object=wl_registry_bind(registry,name,&wl_output_interface,version<4?version:4);
        wl_output_add_listener(output->object,&output_listener,output);
        if (connection->xdg_manager) {
            output->xdg=zxdg_output_manager_v1_get_xdg_output(connection->xdg_manager,output->object);
            zxdg_output_v1_add_listener(output->xdg,&logical_listener,output);
        }
    }
}
static void global_remove(void *data,struct wl_registry *registry,uint32_t name) {
    (void)registry;struct am_kwin *connection=data;
    for (size_t i=0;i<connection->count;i++) if (connection->outputs[i].global==name) {
        connection->outputs[i].removed=true;
        if (i==connection->selected) connection->failed=true;
    }
}
static const struct wl_registry_listener registry_listener={.global=global,.global_remove=global_remove};
int am_kwin_poll(struct am_kwin *connection) {
    if (!connection || connection->failed) return -1;
    while (wl_display_prepare_read(connection->display)!=0) {
        if (wl_display_dispatch_pending(connection->display)<0) return -1;
    }
    if (wl_display_flush(connection->display)<0 && errno!=EAGAIN) { wl_display_cancel_read(connection->display);return -1; }
    struct pollfd socket={.fd=wl_display_get_fd(connection->display),.events=POLLIN};
    int ready=poll(&socket,1,0);
    if (ready>0 && (socket.revents&POLLIN)) {
        if (wl_display_read_events(connection->display)<0) return -1;
    } else {
        wl_display_cancel_read(connection->display);
        if ((ready<0 && errno!=EINTR) || (ready>0 && (socket.revents&(POLLERR|POLLHUP|POLLNVAL)))) return -1;
    }
    return wl_display_dispatch_pending(connection->display)<0 || connection->failed ? -1:0;
}
static void synchronized(void *data,struct wl_callback *callback,uint32_t serial) { (void)callback;(void)serial;*(bool *)data=true; }
static const struct wl_callback_listener sync_listener={.done=synchronized};
static int synchronize(struct am_kwin *connection,const atomic_bool *stopped,int64_t deadline) {
    bool done=false;
    struct wl_callback *callback=wl_display_sync(connection->display);
    if (!callback) return -1;
    wl_callback_add_listener(callback,&sync_listener,&done);
    while (!done && !atomic_load(stopped) && g_get_monotonic_time()<deadline) {
        if (am_kwin_poll(connection)<0) break;
        if (!done) g_usleep(1000);
    }
    wl_callback_destroy(callback);
    return done?0:-1;
}
static void stream_closed(void *data,struct zkde_screencast_stream_unstable_v1 *stream) { (void)stream;((struct am_kwin *)data)->failed=true; }
static void stream_created(void *data,struct zkde_screencast_stream_unstable_v1 *stream,uint32_t node) { (void)stream;((struct am_kwin *)data)->node=node; }
static void stream_failed(void *data,struct zkde_screencast_stream_unstable_v1 *stream,const char *error) { (void)stream;(void)error;((struct am_kwin *)data)->failed=true; }
static void stream_serial(void *data,struct zkde_screencast_stream_unstable_v1 *stream,uint32_t high,uint32_t low) { (void)stream;((struct am_kwin *)data)->serial=((uint64_t)high<<32)|low; }
static const struct zkde_screencast_stream_unstable_v1_listener stream_listener={.closed=stream_closed,.created=stream_created,.failed=stream_failed,.serial=stream_serial};
struct am_kwin *am_kwin_open(const char *output,const atomic_bool *stopped,int64_t deadline) {
    if (!output || !stopped) return NULL;
    struct am_kwin *connection=calloc(1,sizeof(*connection));
    if (!connection) return NULL;
    connection->selected=SIZE_MAX;
    connection->display=wl_display_connect(NULL);
    if (!connection->display) goto failure;
    /* Identity is optional for ordinary capture, but required for binding. */
    (void)am_kwin_peer_identity(wl_display_get_fd(connection->display),connection->compositor_id);
    connection->registry=wl_display_get_registry(connection->display);
    wl_registry_add_listener(connection->registry,&registry_listener,connection);
    if (synchronize(connection,stopped,deadline)<0 || !connection->manager || !connection->xdg_manager) goto failure;
    for (size_t i=0;i<connection->count;i++) if (!connection->outputs[i].xdg) {
        connection->outputs[i].xdg=zxdg_output_manager_v1_get_xdg_output(connection->xdg_manager,connection->outputs[i].object);
        zxdg_output_v1_add_listener(connection->outputs[i].xdg,&logical_listener,&connection->outputs[i]);
    }
    if (synchronize(connection,stopped,deadline)<0) goto failure;
    for (size_t i=0;i<connection->count;i++) if (!connection->outputs[i].removed && strcmp(connection->outputs[i].name,output)==0) {
        if (connection->selected!=SIZE_MAX) goto failure;
        connection->selected=i;
    }
    if (connection->selected==SIZE_MAX) goto failure;
    connection->stream=zkde_screencast_unstable_v1_stream_output(connection->manager,connection->outputs[connection->selected].object,ZKDE_SCREENCAST_UNSTABLE_V1_POINTER_EMBEDDED);
    zkde_screencast_stream_unstable_v1_add_listener(connection->stream,&stream_listener,connection);
    while (!connection->node && !connection->serial && !atomic_load(stopped) && g_get_monotonic_time()<deadline) {
        if (am_kwin_poll(connection)<0) goto failure;
        g_usleep(1000);
    }
    if (!connection->node && !connection->serial) goto failure;
    return connection;
failure:
    am_kwin_close(connection);return NULL;
}
int am_kwin_snapshot(struct am_kwin *connection,uint32_t *node,uint64_t *serial,struct am_kwin_geometry *geometry) {
    if (!connection || !node || !serial || !geometry || connection->failed || connection->selected>=connection->count) return -1;
    char compositor_id[65]={0};
    bool identity_valid=am_kwin_validate_peer_identity(wl_display_get_fd(connection->display),connection->compositor_id,compositor_id)==0;
    int64_t left=INT32_MAX,top=INT32_MAX,right=INT32_MIN,bottom=INT32_MIN;
    for (size_t i=0;i<connection->count;i++) {
        const struct output *output=&connection->outputs[i];
        if (output->removed) continue;
        if (output->geometry_pending || output->mode_pending) return -1;
        if (output->width<=0 || output->height<=0 || output->width>32768 || output->height>32768) return -1;
        if (output->x<left) left=output->x;
        if (output->y<top) top=output->y;
        if ((int64_t)output->x+output->width>right) right=(int64_t)output->x+output->width;
        if ((int64_t)output->y+output->height>bottom) bottom=(int64_t)output->y+output->height;
    }
    if (right<=left || bottom<=top || right-left>65536 || bottom-top>65536) return -1;
    const struct output *selected=&connection->outputs[connection->selected];
    *geometry=(struct am_kwin_geometry){.left=selected->x,.top=selected->y,.width=(uint32_t)selected->width,.height=(uint32_t)selected->height,
        .desktop_left=(int32_t)left,.desktop_top=(int32_t)top,.desktop_width=(uint32_t)(right-left),.desktop_height=(uint32_t)(bottom-top)};
    geometry->registry_global=selected->global; geometry->mode_width=selected->mode_width;
    geometry->mode_height=selected->mode_height; geometry->refresh_millihz=selected->refresh_millihz;
    memcpy(geometry->output_name,selected->name,sizeof(geometry->output_name));
    memset(geometry->compositor_id,0,sizeof(geometry->compositor_id));
    if (identity_valid && am_kwin_validate_peer_identity(wl_display_get_fd(connection->display),connection->compositor_id,compositor_id)==0)
        memcpy(geometry->compositor_id,compositor_id,sizeof(geometry->compositor_id));
    *node=connection->node;*serial=connection->serial;
    return 0;
}
void am_kwin_close(struct am_kwin *connection) {
    if (!connection) return;
    if (connection->stream) zkde_screencast_stream_unstable_v1_close(connection->stream);
    for (size_t i=0;i<connection->count;i++) {
        struct output *output=&connection->outputs[i];
        release_output(output);
    }
    if (connection->manager) zkde_screencast_unstable_v1_destroy(connection->manager);
    if (connection->xdg_manager) zxdg_output_manager_v1_destroy(connection->xdg_manager);
    if (connection->registry) wl_registry_destroy(connection->registry);
    if (connection->display) { wl_display_flush(connection->display);wl_display_disconnect(connection->display); }
    free(connection);
}

__attribute__((used)) const char amaterasu_kwin_marker[] = "AMATERASU_KWIN_CAPTURE=1";
