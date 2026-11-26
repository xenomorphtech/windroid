#define _GNU_SOURCE
// libandroid replacement for the wwd runtime:
//   ALooper / AInputQueue / AInputEvent  (real fds, so poll() works)
//   ANativeWindow                         (backed by a host window)
//   AAssetManager / AAsset / AAssetDir    (reads the installed split APKs)
//   AConfiguration, ANativeActivity_*, and stubs for hardware buffers.
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include "jvm.h"
#include "wwd_host.h"

#define EXPORT __attribute__((visibility("default")))

// ---------------------------------------------------------------------------
// ALooper
// ---------------------------------------------------------------------------

typedef int (*ALooper_callbackFunc)(int fd, int events, void *data);
#define ALOOPER_POLL_WAKE (-1)
#define ALOOPER_POLL_CALLBACK (-2)
#define ALOOPER_POLL_TIMEOUT (-3)
#define ALOOPER_POLL_ERROR (-4)
#define ALOOPER_EVENT_INPUT 1
#define ALOOPER_EVENT_OUTPUT 2
#define ALOOPER_EVENT_ERROR 4
#define ALOOPER_EVENT_HANGUP 8

typedef struct {
    int fd, ident, events;
    ALooper_callbackFunc cb;
    void *data;
} looper_fd;

typedef struct ALooper {
    pthread_mutex_t lock;
    looper_fd fds[32];
    int n;
    int wake[2];
} ALooper;

static __thread ALooper *t_looper;

static ALooper *looper_new(void) {
    ALooper *l = calloc(1, sizeof *l);
    pthread_mutex_init(&l->lock, NULL);
    pipe2(l->wake, O_CLOEXEC | O_NONBLOCK);
    return l;
}

EXPORT ALooper *ALooper_forThread(void) { return t_looper; }
EXPORT ALooper *ALooper_prepare(int opts) {
    (void)opts;
    if (!t_looper) t_looper = looper_new();
    return t_looper;
}
EXPORT void ALooper_acquire(ALooper *l) { (void)l; }
EXPORT void ALooper_release(ALooper *l) { (void)l; }
EXPORT void ALooper_wake(ALooper *l) {
    char c = 1;
    if (l) (void)!write(l->wake[1], &c, 1);
}

EXPORT int ALooper_addFd(ALooper *l, int fd, int ident, int events, ALooper_callbackFunc cb, void *data) {
    if (!l) return -1;
    pthread_mutex_lock(&l->lock);
    int i;
    for (i = 0; i < l->n; i++)
        if (l->fds[i].fd == fd) break;
    if (i == l->n) {
        if (l->n == 32) {
            pthread_mutex_unlock(&l->lock);
            return -1;
        }
        l->n++;
    }
    l->fds[i] = (looper_fd){fd, cb ? ALOOPER_POLL_CALLBACK : ident, events ? events : ALOOPER_EVENT_INPUT, cb, data};
    pthread_mutex_unlock(&l->lock);
    ALooper_wake(l);
    return 1;
}

EXPORT int ALooper_removeFd(ALooper *l, int fd) {
    if (!l) return -1;
    pthread_mutex_lock(&l->lock);
    for (int i = 0; i < l->n; i++)
        if (l->fds[i].fd == fd) {
            l->fds[i] = l->fds[--l->n];
            pthread_mutex_unlock(&l->lock);
            return 1;
        }
    pthread_mutex_unlock(&l->lock);
    return 0;
}

EXPORT int ALooper_pollOnce(int timeout_ms, int *out_fd, int *out_events, void **out_data) {
    ALooper *l = ALooper_prepare(0);
    uint64_t deadline = timeout_ms < 0 ? 0 : wwd_host_monotonic_ns() + (uint64_t)timeout_ms * 1000000ull;
    for (;;) {
        struct pollfd pfd[33];
        looper_fd snap[32];
        pthread_mutex_lock(&l->lock);
        int n = l->n;
        memcpy(snap, l->fds, sizeof(looper_fd) * (size_t)n);
        pthread_mutex_unlock(&l->lock);
        for (int i = 0; i < n; i++) pfd[i] = (struct pollfd){snap[i].fd, POLLIN, 0};
        pfd[n] = (struct pollfd){l->wake[0], POLLIN, 0};
        int wait = -1;
        if (timeout_ms >= 0) {
            uint64_t now = wwd_host_monotonic_ns();
            wait = now >= deadline ? 0 : (int)((deadline - now) / 1000000ull);
        }
        int r = poll(pfd, (nfds_t)n + 1, wait);
        if (r < 0) {
            if (errno == EINTR) continue;
            return ALOOPER_POLL_ERROR;
        }
        if (r == 0) return ALOOPER_POLL_TIMEOUT;
        if (pfd[n].revents) {
            char buf[64];
            while (read(l->wake[0], buf, sizeof buf) > 0) {}
            return ALOOPER_POLL_WAKE;
        }
        int called = 0;
        for (int i = 0; i < n; i++) {
            if (!pfd[i].revents) continue;
            int ev = (pfd[i].revents & POLLIN ? ALOOPER_EVENT_INPUT : 0) | (pfd[i].revents & POLLHUP ? ALOOPER_EVENT_HANGUP : 0);
            if (snap[i].cb) {
                if (!snap[i].cb(snap[i].fd, ev, snap[i].data)) ALooper_removeFd(l, snap[i].fd);
                called = 1;
                continue;
            }
            if (out_fd) *out_fd = snap[i].fd;
            if (out_events) *out_events = ev;
            if (out_data) *out_data = snap[i].data;
            return snap[i].ident;
        }
        if (called) return ALOOPER_POLL_CALLBACK;
    }
}

EXPORT int ALooper_pollAll(int timeout_ms, int *out_fd, int *out_events, void **out_data) {
    for (;;) {
        int r = ALooper_pollOnce(timeout_ms, out_fd, out_events, out_data);
        if (r != ALOOPER_POLL_CALLBACK) return r;
    }
}

// ---------------------------------------------------------------------------
// input
// ---------------------------------------------------------------------------

typedef struct AInputEvent {
    int32_t type; // 1 key, 2 motion
    int32_t source, device, action, flags, meta, keycode, repeat, buttons;
    float x, y;
    int64_t time, down_time;
    struct AInputEvent *next;
} AInputEvent;

typedef struct AInputQueue {
    pthread_mutex_t lock;
    int pipe[2];
    AInputEvent *head, *tail;
    ALooper *looper;
} AInputQueue;

EXPORT AInputQueue *wwd_input_queue_create(void) {
    AInputQueue *q = calloc(1, sizeof *q);
    pthread_mutex_init(&q->lock, NULL);
    pipe2(q->pipe, O_CLOEXEC | O_NONBLOCK);
    return q;
}

static int64_t now_ns(void) { return (int64_t)wwd_host_monotonic_ns(); }
static int64_t g_down_time;

EXPORT void wwd_input_queue_push(AInputQueue *q, int type, int action, float x, float y, int keycode) {
    AInputEvent *e = calloc(1, sizeof *e);
    e->type = type;
    e->action = action;
    e->x = x;
    e->y = y;
    e->keycode = keycode;
    // Device id 0: UE drops touch/key events from device ids it hasn't seen
    // registered through Java's InputManager scan (FAndroidInputInterface
    // device map); id 0 bypasses that lookup.
    e->device = 0;
    e->source = type == 2 ? 0x1002 /* touchscreen */ : 0x101 /* keyboard */;
    if (type == 2 && getenv("WWD_INPUT_MOUSE")) { // experiment: deliver as a mouse
        e->source = 0x2002;
        e->buttons = action == 1 ? 0 : 1;
    }
    if (getenv("WWD_INPUT_DEVICE")) e->device = atoi(getenv("WWD_INPUT_DEVICE")); // debug override
    e->time = now_ns();
    if (type == 2 && action == 0) g_down_time = e->time;
    e->down_time = type == 2 ? g_down_time : e->time;
    pthread_mutex_lock(&q->lock);
    if (q->tail) q->tail->next = e; else q->head = e;
    q->tail = e;
    pthread_mutex_unlock(&q->lock);
    char c = 1;
    (void)!write(q->pipe[1], &c, 1);
}

EXPORT void AInputQueue_attachLooper(AInputQueue *q, ALooper *l, int ident, ALooper_callbackFunc cb, void *data) {
    q->looper = l;
    ALooper_addFd(l, q->pipe[0], ident, ALOOPER_EVENT_INPUT, cb, data);
}
EXPORT void AInputQueue_detachLooper(AInputQueue *q) {
    if (q->looper) ALooper_removeFd(q->looper, q->pipe[0]);
    q->looper = NULL;
}
EXPORT int32_t AInputQueue_hasEvents(AInputQueue *q) { return q->head ? 1 : 0; }
EXPORT int32_t AInputQueue_getEvent(AInputQueue *q, AInputEvent **out) {
    pthread_mutex_lock(&q->lock);
    AInputEvent *e = q->head;
    if (e) {
        q->head = e->next;
        if (!q->head) q->tail = NULL;
    }
    pthread_mutex_unlock(&q->lock);
    char c;
    (void)!read(q->pipe[0], &c, 1);
    if (!e) return -EAGAIN;
    if (getenv("WWD_INPUT_TRACE")) wwd_logf("wwd-input", "deliver type %d action %d at %.0f,%.0f", e->type, e->action, e->x, e->y);
    *out = e;
    return 0;
}
EXPORT int32_t AInputQueue_preDispatchEvent(AInputQueue *q, AInputEvent *e) { (void)q; (void)e; return 0; }
EXPORT void AInputQueue_finishEvent(AInputQueue *q, AInputEvent *e, int handled) { (void)q; (void)handled; free(e); }

EXPORT int32_t AInputEvent_getType(const AInputEvent *e) { return e->type; }
EXPORT int32_t AInputEvent_getDeviceId(const AInputEvent *e) { return e->device; }
EXPORT int32_t AInputEvent_getSource(const AInputEvent *e) { return e->source; }
EXPORT void AInputEvent_release(const AInputEvent *e) { (void)e; }
EXPORT int32_t AKeyEvent_getAction(const AInputEvent *e) { return e->action; }
EXPORT int32_t AKeyEvent_getFlags(const AInputEvent *e) { return e->flags; }
EXPORT int32_t AKeyEvent_getKeyCode(const AInputEvent *e) { return e->keycode; }
EXPORT int32_t AKeyEvent_getScanCode(const AInputEvent *e) { return e->keycode; }
EXPORT int32_t AKeyEvent_getMetaState(const AInputEvent *e) { return e->meta; }
EXPORT int32_t AKeyEvent_getRepeatCount(const AInputEvent *e) { return e->repeat; }
EXPORT int64_t AKeyEvent_getDownTime(const AInputEvent *e) { return e->down_time; }
EXPORT int64_t AKeyEvent_getEventTime(const AInputEvent *e) { return e->time; }
EXPORT int32_t AMotionEvent_getAction(const AInputEvent *e) { return e->action; }
EXPORT int32_t AMotionEvent_getFlags(const AInputEvent *e) { return e->flags; }
EXPORT int32_t AMotionEvent_getMetaState(const AInputEvent *e) { return e->meta; }
EXPORT int32_t AMotionEvent_getButtonState(const AInputEvent *e) { return e->buttons; }
EXPORT int32_t AMotionEvent_getEdgeFlags(const AInputEvent *e) { (void)e; return 0; }
EXPORT int64_t AMotionEvent_getDownTime(const AInputEvent *e) { return e->down_time; }
EXPORT int64_t AMotionEvent_getEventTime(const AInputEvent *e) { return e->time; }
EXPORT size_t AMotionEvent_getPointerCount(const AInputEvent *e) { (void)e; return 1; }
EXPORT int32_t AMotionEvent_getPointerId(const AInputEvent *e, size_t i) { (void)e; (void)i; return 0; }
EXPORT int32_t AMotionEvent_getToolType(const AInputEvent *e, size_t i) { (void)i; return e->source == 0x2002 ? 3 : 1; /* mouse : finger */ }
EXPORT float AMotionEvent_getX(const AInputEvent *e, size_t i) {
    (void)i;
    if (getenv("WWD_INPUT_TRACE")) {
        Dl_info di;
        void *ra = __builtin_return_address(0);
        if (dladdr(ra, &di)) wwd_logf("wwd-input", "getX from %s+%#lx", di.dli_fname, (unsigned long)((char *)ra - (char *)di.dli_fbase));
        // caller's stack (its sp is a little above our frame record): ints
        int *fp = (int *)__builtin_frame_address(0);
        char line[512]; int o = 0;
        for (int k = 0; k < 40 && o < 480; k++) o += snprintf(line + o, sizeof line - o, "%d ", fp[k]);
        wwd_logf("wwd-input", "stack ints: %s", line);
    }
    return e->x;
}
EXPORT float AMotionEvent_getY(const AInputEvent *e, size_t i) { (void)i; return e->y; }
EXPORT float AMotionEvent_getRawX(const AInputEvent *e, size_t i) { (void)i; return e->x; }
EXPORT float AMotionEvent_getRawY(const AInputEvent *e, size_t i) { (void)i; return e->y; }
EXPORT float AMotionEvent_getPressure(const AInputEvent *e, size_t i) { (void)e; (void)i; return 1.0f; }
EXPORT float AMotionEvent_getSize(const AInputEvent *e, size_t i) { (void)e; (void)i; return 0.1f; }
EXPORT float AMotionEvent_getAxisValue(const AInputEvent *e, int32_t axis, size_t i) { (void)i; return axis == 0 ? e->x : axis == 1 ? e->y : 0.0f; }
EXPORT float AMotionEvent_getXPrecision(const AInputEvent *e) { (void)e; return 1.0f; }
EXPORT float AMotionEvent_getYPrecision(const AInputEvent *e) { (void)e; return 1.0f; }
EXPORT size_t AMotionEvent_getHistorySize(const AInputEvent *e) { (void)e; return 0; }

// ---------------------------------------------------------------------------
// ANativeWindow
// ---------------------------------------------------------------------------

typedef struct ANativeWindow ANativeWindow;
static ANativeWindow *g_window;

EXPORT ANativeWindow *wwd_native_window_create(uint32_t w, uint32_t h) {
    ANativeWindow *win = calloc(1, sizeof *win);
    win->magic = WWD_WINDOW_MAGIC;
    win->refs = 1;
    win->host_window = wwd_host_window_create(w, h);
    win->width = (int32_t)w;
    win->height = (int32_t)h;
    win->format = 1; // RGBA_8888
    g_window = win;
    return win;
}
EXPORT void ANativeWindow_acquire(ANativeWindow *w) { if (w) __atomic_add_fetch(&w->refs, 1, __ATOMIC_SEQ_CST); }
EXPORT void ANativeWindow_release(ANativeWindow *w) { if (w) __atomic_sub_fetch(&w->refs, 1, __ATOMIC_SEQ_CST); }
static void refresh(ANativeWindow *w) {
    uint32_t ww = 0, hh = 0;
    wwd_host_window_size(w->host_window, &ww, &hh);
    if (ww && hh) {
        w->width = (int32_t)ww;
        w->height = (int32_t)hh;
    }
}
EXPORT int32_t ANativeWindow_getWidth(ANativeWindow *w) { if (!w) return -1; refresh(w); return w->width; }
EXPORT int32_t ANativeWindow_getHeight(ANativeWindow *w) { if (!w) return -1; refresh(w); return w->height; }
EXPORT int32_t ANativeWindow_getFormat(ANativeWindow *w) { return w ? w->format : -1; }
EXPORT int32_t ANativeWindow_setBuffersGeometry(ANativeWindow *w, int32_t width, int32_t height, int32_t format) {
    (void)width; (void)height;
    if (w && format) w->format = format;
    return 0;
}
EXPORT int32_t ANativeWindow_setBuffersTransform(ANativeWindow *w, int32_t t) { (void)w; (void)t; return 0; }
EXPORT int32_t ANativeWindow_setFrameRate(ANativeWindow *w, float r, int8_t c) { (void)w; (void)r; (void)c; return 0; }
EXPORT ANativeWindow *ANativeWindow_fromSurface(void *env, void *surface) { (void)env; (void)surface; if (g_window) ANativeWindow_acquire(g_window); return g_window; }
EXPORT int32_t ANativeWindow_lock(ANativeWindow *w, void *buf, void *dirty) { (void)w; (void)buf; (void)dirty; return -1; }
EXPORT int32_t ANativeWindow_unlockAndPost(ANativeWindow *w) { (void)w; return -1; }

// ---------------------------------------------------------------------------
// assets: zip central directories of every installed APK
// ---------------------------------------------------------------------------

typedef struct z_stream_s {
    const unsigned char *next_in;
    unsigned avail_in;
    unsigned long total_in;
    unsigned char *next_out;
    unsigned avail_out;
    unsigned long total_out;
    const char *msg;
    void *state, *zalloc, *zfree, *opaque;
    int data_type;
    unsigned long adler, reserved;
} z_stream;
int inflateInit2_(z_stream *, int, const char *, int);
int inflate(z_stream *, int);
int inflateEnd(z_stream *);

typedef struct {
    char *name; // path below assets/
    int apk;
    int method;
    uint64_t csize, usize, data_off;
} asset_entry;

typedef struct AAssetManager {
    char *apk_paths[16];
    int apk_fds[16];
    int napk;
    asset_entry *entries;
    size_t n, cap;
} AAssetManager;

static AAssetManager g_assets;
static char g_override_dir[512];

static uint16_t rd16(const unsigned char *p) { return (uint16_t)(p[0] | p[1] << 8); }
static uint32_t rd32(const unsigned char *p) { return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24; }

static void index_apk(AAssetManager *m, const char *path) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return;
    off_t size = lseek(fd, 0, SEEK_END);
    size_t tail = size < 65557 ? (size_t)size : 65557;
    unsigned char *buf = malloc(tail);
    pread(fd, buf, tail, size - (off_t)tail);
    long eocd = -1;
    for (long i = (long)tail - 22; i >= 0; i--)
        if (rd32(buf + i) == 0x06054b50) { eocd = i; break; }
    if (eocd < 0) { free(buf); close(fd); return; }
    uint32_t cd_size = rd32(buf + eocd + 12), cd_off = rd32(buf + eocd + 16);
    free(buf);
    unsigned char *cd = malloc(cd_size);
    pread(fd, cd, cd_size, cd_off);
    int apk = m->napk++;
    m->apk_paths[apk] = strdup(path);
    m->apk_fds[apk] = fd;
    for (uint32_t o = 0; o + 46 <= cd_size && rd32(cd + o) == 0x02014b50;) {
        uint16_t method = rd16(cd + o + 10), nlen = rd16(cd + o + 28), elen = rd16(cd + o + 30), clen = rd16(cd + o + 32);
        uint32_t csize = rd32(cd + o + 20), usize = rd32(cd + o + 24), lho = rd32(cd + o + 42);
        const char *name = (const char *)cd + o + 46;
        if (nlen > 7 && !memcmp(name, "assets/", 7)) {
            unsigned char lh[30];
            pread(fd, lh, 30, lho);
            uint64_t data = lho + 30 + rd16(lh + 26) + rd16(lh + 28);
            if (m->n == m->cap) {
                m->cap = m->cap ? m->cap * 2 : 1024;
                m->entries = realloc(m->entries, m->cap * sizeof(asset_entry));
            }
            asset_entry *e = &m->entries[m->n++];
            e->name = strndup(name + 7, nlen - 7);
            e->apk = apk;
            e->method = method;
            e->csize = csize;
            e->usize = usize;
            e->data_off = data;
        }
        o += 46 + nlen + elen + clen;
    }
    free(cd);
}

EXPORT void wwd_assets_init(const char *pkg_dir) {
    char path[1024];
    snprintf(g_override_dir, sizeof g_override_dir, "%s/asset_overrides", pkg_dir);
    snprintf(path, sizeof path, "%s/base.apk", pkg_dir);
    index_apk(&g_assets, path);
    DIR *d = opendir(pkg_dir);
    if (d) {
        struct dirent *de;
        while ((de = readdir(d))) {
            size_t n = strlen(de->d_name);
            if (n > 4 && !strcmp(de->d_name + n - 4, ".apk") && strcmp(de->d_name, "base.apk")) {
                snprintf(path, sizeof path, "%s/%s", pkg_dir, de->d_name);
                index_apk(&g_assets, path);
            }
        }
        closedir(d);
    }
    wwd_logf("wwd-assets", "%zu assets in %d APKs", g_assets.n, g_assets.napk);
}

EXPORT AAssetManager *wwd_asset_manager(void) { return &g_assets; }
EXPORT AAssetManager *AAssetManager_fromJava(void *env, void *obj) { (void)env; (void)obj; return &g_assets; }

static asset_entry *find_asset(AAssetManager *m, const char *name) {
    while (name[0] == '/') name++;
    for (size_t i = 0; i < m->n; i++)
        if (!strcmp(m->entries[i].name, name)) return &m->entries[i];
    return NULL;
}

typedef struct AAsset {
    AAssetManager *mgr;
    asset_entry *e;
    uint64_t pos;
    unsigned char *buf; // inflated data (deflated entries) or getBuffer() copy
} AAsset;

// <pkg>/asset_overrides/<name> replaces an APK asset (whole-file, read into memory)
static AAsset *open_override(AAssetManager *m, const char *name) {
    if (!g_override_dir[0]) return NULL;
    while (name[0] == '/') name++;
    char path[1024];
    snprintf(path, sizeof path, "%s/%s", g_override_dir, name);
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    AAsset *a = calloc(1, sizeof *a);
    asset_entry *e = calloc(1, sizeof *e);
    e->name = strdup(name);
    e->method = 99; // in-memory
    e->usize = (uint64_t)n;
    a->mgr = m;
    a->e = e;
    a->buf = malloc((size_t)n + 1);
    if (fread(a->buf, 1, (size_t)n, f) != (size_t)n) n = 0;
    fclose(f);
    wwd_logf("wwd-assets", "asset %s served from override (%ld bytes)", name, n);
    return a;
}

EXPORT AAsset *AAssetManager_open(AAssetManager *m, const char *name, int mode) {
    (void)mode;
    AAsset *ov = open_override(m, name);
    if (ov) return ov;
    asset_entry *e = find_asset(m, name);
    if (!e) {
        if (wwd_jni_trace) wwd_logf("wwd-assets", "open %s: not found", name);
        return NULL;
    }
    AAsset *a = calloc(1, sizeof *a);
    a->mgr = m;
    a->e = e;
    if (e->method == 8) {
        unsigned char *src = malloc(e->csize);
        pread(m->apk_fds[e->apk], src, e->csize, (off_t)e->data_off);
        a->buf = malloc(e->usize + 1);
        z_stream zs;
        memset(&zs, 0, sizeof zs);
        inflateInit2_(&zs, -15, "1.2.13", (int)sizeof(z_stream));
        zs.next_in = src;
        zs.avail_in = (unsigned)e->csize;
        zs.next_out = a->buf;
        zs.avail_out = (unsigned)e->usize;
        inflate(&zs, 4 /* Z_FINISH */);
        inflateEnd(&zs);
        free(src);
    }
    return a;
}

EXPORT int AAsset_read(AAsset *a, void *buf, size_t n) {
    uint64_t left = a->e->usize - a->pos;
    if (n > left) n = (size_t)left;
    if (!n) return 0;
    if (a->buf) memcpy(buf, a->buf + a->pos, n);
    else if (pread(a->mgr->apk_fds[a->e->apk], buf, n, (off_t)(a->e->data_off + a->pos)) != (ssize_t)n) return -1;
    a->pos += n;
    return (int)n;
}
EXPORT int64_t AAsset_seek64(AAsset *a, int64_t off, int whence) {
    int64_t np = whence == SEEK_SET ? off : whence == SEEK_CUR ? (int64_t)a->pos + off : (int64_t)a->e->usize + off;
    if (np < 0 || (uint64_t)np > a->e->usize) return -1;
    a->pos = (uint64_t)np;
    return np;
}
EXPORT long AAsset_seek(AAsset *a, long off, int whence) { return (long)AAsset_seek64(a, off, whence); }
EXPORT int64_t AAsset_getLength64(AAsset *a) { return (int64_t)a->e->usize; }
EXPORT long AAsset_getLength(AAsset *a) { return (long)a->e->usize; }
EXPORT int64_t AAsset_getRemainingLength64(AAsset *a) { return (int64_t)(a->e->usize - a->pos); }
EXPORT long AAsset_getRemainingLength(AAsset *a) { return (long)(a->e->usize - a->pos); }
EXPORT int AAsset_isAllocated(AAsset *a) { return a->buf != NULL; }
EXPORT const void *AAsset_getBuffer(AAsset *a) {
    if (!a->buf) {
        a->buf = malloc(a->e->usize + 1);
        pread(a->mgr->apk_fds[a->e->apk], a->buf, a->e->usize, (off_t)a->e->data_off);
    }
    return a->buf;
}
EXPORT int AAsset_openFileDescriptor64(AAsset *a, int64_t *start, int64_t *len) {
    if (a->e->method != 0) return -1; // only stored entries can be exposed as a file range
    int fd = open(a->mgr->apk_paths[a->e->apk], O_RDONLY | O_CLOEXEC);
    if (fd >= 0) {
        *start = (int64_t)a->e->data_off;
        *len = (int64_t)a->e->usize;
    }
    return fd;
}
EXPORT int AAsset_openFileDescriptor(AAsset *a, off_t *start, off_t *len) {
    int64_t s, l;
    int fd = AAsset_openFileDescriptor64(a, &s, &l);
    if (fd >= 0) {
        *start = (off_t)s;
        *len = (off_t)l;
    }
    return fd;
}
EXPORT void AAsset_close(AAsset *a) {
    if (!a) return;
    free(a->buf);
    free(a);
}

typedef struct AAssetDir {
    AAssetManager *m;
    char *dir;
    size_t idx;
    char name[512];
} AAssetDir;

EXPORT AAssetDir *AAssetManager_openDir(AAssetManager *m, const char *dir) {
    AAssetDir *d = calloc(1, sizeof *d);
    d->m = m;
    while (dir[0] == '/') dir++;
    d->dir = strdup(dir);
    size_t n = strlen(d->dir);
    if (n && d->dir[n - 1] == '/') d->dir[n - 1] = 0;
    return d;
}
EXPORT const char *AAssetDir_getNextFileName(AAssetDir *d) {
    size_t dl = strlen(d->dir);
    while (d->idx < d->m->n) {
        const char *nm = d->m->entries[d->idx++].name;
        const char *rest;
        if (dl == 0) rest = nm;
        else if (!strncmp(nm, d->dir, dl) && nm[dl] == '/') rest = nm + dl + 1;
        else continue;
        if (strchr(rest, '/')) continue; // files only, like Android
        snprintf(d->name, sizeof d->name, "%s", rest);
        return d->name;
    }
    return NULL;
}
EXPORT void AAssetDir_rewind(AAssetDir *d) { d->idx = 0; }
EXPORT void AAssetDir_close(AAssetDir *d) {
    if (!d) return;
    free(d->dir);
    free(d);
}

// ---------------------------------------------------------------------------
// AConfiguration
// ---------------------------------------------------------------------------

typedef struct AConfiguration {
    char lang[2], country[2];
    int32_t density, orientation, sdk, touchscreen, keyboard, navigation, screen_size, ui_mode;
} AConfiguration;

EXPORT AConfiguration *AConfiguration_new(void) {
    AConfiguration *c = calloc(1, sizeof *c);
    memcpy(c->lang, "en", 2);
    memcpy(c->country, "US", 2);
    c->density = 320;
    c->orientation = 2; // landscape
    c->sdk = 34;
    c->touchscreen = 3;
    c->keyboard = 2;
    c->navigation = 1;
    c->screen_size = 3;
    c->ui_mode = 1;
    return c;
}
EXPORT void AConfiguration_delete(AConfiguration *c) { free(c); }
EXPORT void AConfiguration_fromAssetManager(AConfiguration *out, AAssetManager *m) {
    (void)m;
    AConfiguration *d = AConfiguration_new();
    *out = *d;
    free(d);
}
EXPORT void AConfiguration_getLanguage(AConfiguration *c, char *out) { memcpy(out, c->lang, 2); }
EXPORT void AConfiguration_getCountry(AConfiguration *c, char *out) { memcpy(out, c->country, 2); }
EXPORT int32_t AConfiguration_getDensity(AConfiguration *c) { return c->density; }
EXPORT int32_t AConfiguration_getOrientation(AConfiguration *c) { return c->orientation; }
EXPORT int32_t AConfiguration_getSdkVersion(AConfiguration *c) { return c->sdk; }
EXPORT int32_t AConfiguration_getTouchscreen(AConfiguration *c) { return c->touchscreen; }
EXPORT int32_t AConfiguration_getKeyboard(AConfiguration *c) { return c->keyboard; }
EXPORT int32_t AConfiguration_getNavigation(AConfiguration *c) { return c->navigation; }
EXPORT int32_t AConfiguration_getScreenSize(AConfiguration *c) { return c->screen_size; }
EXPORT int32_t AConfiguration_getUiModeType(AConfiguration *c) { return c->ui_mode; }
EXPORT int32_t AConfiguration_getScreenWidthDp(AConfiguration *c) { (void)c; return 960; }
EXPORT int32_t AConfiguration_getScreenHeightDp(AConfiguration *c) { (void)c; return 540; }

// ---------------------------------------------------------------------------
// misc
// ---------------------------------------------------------------------------

typedef struct ANativeActivity ANativeActivity;
EXPORT void ANativeActivity_setWindowFormat(ANativeActivity *a, int32_t f) { (void)a; (void)f; }
EXPORT void ANativeActivity_setWindowFlags(ANativeActivity *a, uint32_t add, uint32_t rm) { (void)a; (void)add; (void)rm; }
EXPORT void ANativeActivity_showSoftInput(ANativeActivity *a, uint32_t f) { (void)a; (void)f; }
EXPORT void ANativeActivity_hideSoftInput(ANativeActivity *a, uint32_t f) { (void)a; (void)f; }
EXPORT void ANativeActivity_finish(ANativeActivity *a) {
    (void)a;
    wwd_logf("wwd", "ANativeActivity_finish: exiting");
    exit(0);
}

EXPORT void AHardwareBuffer_acquire(void *b) { (void)b; }
EXPORT void AHardwareBuffer_release(void *b) { (void)b; }
EXPORT void AHardwareBuffer_describe(const void *b, void *desc) { (void)b; if (desc) memset(desc, 0, 40); }
EXPORT void *AHardwareBuffer_fromHardwareBuffer(void *env, void *obj) { (void)env; (void)obj; return NULL; }
EXPORT int AHardwareBuffer_allocate(const void *desc, void **out) { (void)desc; if (out) *out = NULL; return -12; }

EXPORT int ASharedMemory_create(const char *name, size_t size) {
    // backed by an unlinked temp file; good enough for single-process use
    char path[256];
    static int seq;
    snprintf(path, sizeof path, "/data/local/tmp/.ashmem-%d-%d", getpid(), __atomic_add_fetch(&seq, 1, __ATOMIC_SEQ_CST));
    int fd = open(path, O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (fd < 0) return -1;
    unlink(path);
    if (ftruncate(fd, (off_t)size) < 0) {
        close(fd);
        return -1;
    }
    (void)name;
    return fd;
}
EXPORT size_t ASharedMemory_getSize(int fd) {
    off_t cur = lseek(fd, 0, SEEK_CUR), end = lseek(fd, 0, SEEK_END);
    lseek(fd, cur, SEEK_SET);
    return end < 0 ? 0 : (size_t)end;
}
EXPORT int ASharedMemory_setProt(int fd, int prot) { (void)fd; (void)prot; return 0; }
