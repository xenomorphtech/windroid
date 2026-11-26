// OpenSL ES for wwd: Engine, OutputMix and AudioPlayer objects with the
// Android simple buffer queue, Play, Volume and AndroidConfiguration
// interfaces. Enqueued PCM is consumed in real time by a player thread
// (driving the app's buffer-queue callback like a real device would).
// Output currently goes to a null sink; the timing is what apps depend on.
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "wwd_host.h"

typedef uint32_t SLresult, SLuint32, SLboolean;
typedef int32_t SLint32;
typedef int16_t SLmillibel;
struct SLInterfaceID_ { uint32_t time_low; uint16_t time_mid, time_hi_and_version, clock_seq; uint8_t node[6]; };
typedef const struct SLInterfaceID_ *SLInterfaceID;

#define SL_RESULT_SUCCESS 0
#define SL_RESULT_PARAMETER_INVALID 2
#define SL_RESULT_FEATURE_UNSUPPORTED 0x0C
#define SL_OBJECT_STATE_REALIZED 2
#define SL_PLAYSTATE_STOPPED 1
#define SL_PLAYSTATE_PAUSED 2
#define SL_PLAYSTATE_PLAYING 3

#define IID(name, a) static const struct SLInterfaceID_ name##_s = {a, 0x5757, 0, 0, {0}}; const SLInterfaceID name = &name##_s;
IID(SL_IID_NULL, 0)
IID(SL_IID_OBJECT, 1)
IID(SL_IID_ENGINE, 2)
IID(SL_IID_PLAY, 3)
IID(SL_IID_RECORD, 4)
IID(SL_IID_VOLUME, 5)
IID(SL_IID_BUFFERQUEUE, 6)
IID(SL_IID_ANDROIDSIMPLEBUFFERQUEUE, 7)
IID(SL_IID_ANDROIDCONFIGURATION, 8)
IID(SL_IID_ENVIRONMENTALREVERB, 9)
IID(SL_IID_EFFECTSEND, 10)
IID(SL_IID_SEEK, 11)
IID(SL_IID_MUTESOLO, 12)
IID(SL_IID_PLAYBACKRATE, 13)
IID(SL_IID_PREFETCHSTATUS, 14)
IID(SL_IID_METADATAEXTRACTION, 15)
IID(SL_IID_ANDROIDEFFECT, 16)
IID(SL_IID_ANDROIDBUFFERQUEUESOURCE, 17)
IID(SL_IID_ENGINECAPABILITIES, 18)
IID(SL_IID_OUTPUTMIX, 19)

// ---------------------------------------------------------------------------
// object model: every interface pointer points at a struct whose first field
// is the vtable pointer, followed by a back-pointer to the owning object.
// ---------------------------------------------------------------------------

struct obj;
typedef void (*bq_callback)(const void *caller, void *ctx);

typedef struct { const void *vtbl; struct obj *o; } itf;

typedef struct obj {
    itf object, engine, play, bq, volume, config;
    int kind; // 1 engine, 2 output mix, 3 player
    // player state
    pthread_t thread;
    pthread_mutex_t lock;
    pthread_cond_t cv;
    int thread_started, quit;
    SLuint32 play_state;
    struct { const void *buf; SLuint32 size; } q[64];
    int q_head, q_count, q_cap;
    SLuint32 played;
    bq_callback cb;
    void *cb_ctx;
    SLuint32 bytes_per_sec;
} obj;

#define SELF(type) obj *o = (*(itf *const *)self)->o
static obj *from(const void *self) { return ((const itf *)self)->o; }

// ---- SLObjectItf ----
static SLresult Object_Realize(const void *self, SLboolean async) { (void)self; (void)async; return SL_RESULT_SUCCESS; }
static SLresult Object_Resume(const void *self, SLboolean async) { (void)self; (void)async; return SL_RESULT_SUCCESS; }
static SLresult Object_GetState(const void *self, SLuint32 *st) { (void)self; if (st) *st = SL_OBJECT_STATE_REALIZED; return SL_RESULT_SUCCESS; }
static SLresult Object_GetInterface(const void *self, SLInterfaceID iid, void *out);
static SLresult Object_RegisterCallback(const void *self, void *cb, void *ctx) { (void)self; (void)cb; (void)ctx; return SL_RESULT_SUCCESS; }
static void Object_AbortAsyncOperation(const void *self) { (void)self; }
static void Object_Destroy(const void *self);
static SLresult Object_SetPriority(const void *self, SLint32 p, SLboolean pre) { (void)self; (void)p; (void)pre; return SL_RESULT_SUCCESS; }
static SLresult Object_GetPriority(const void *self, SLint32 *p, SLboolean *pre) { (void)self; if (p) *p = 0; if (pre) *pre = 0; return SL_RESULT_SUCCESS; }
static SLresult Object_SetLossOfControlInterfaces(const void *self, SLint32 n, SLInterfaceID *ids, SLboolean e) { (void)self; (void)n; (void)ids; (void)e; return SL_RESULT_SUCCESS; }
static const void *object_vtbl[] = {Object_Realize, Object_Resume, Object_GetState, Object_GetInterface, Object_RegisterCallback,
                                    Object_AbortAsyncOperation, Object_Destroy, Object_SetPriority, Object_GetPriority,
                                    Object_SetLossOfControlInterfaces};

static obj *new_obj(int kind);

// ---- SLEngineItf ----
static SLresult unsupported(void) { return SL_RESULT_FEATURE_UNSUPPORTED; }

typedef struct { SLuint32 formatType, numChannels, samplesPerSec, bitsPerSample, containerSize, channelMask, endianness; } pcm_format;
typedef struct { void *pLocator; void *pFormat; } data_source;

static SLresult Engine_CreateAudioPlayer(const void *self, const void **out, data_source *src, void *sink, SLuint32 n, const SLInterfaceID *ids, const SLboolean *req) {
    (void)self; (void)sink; (void)n; (void)ids; (void)req;
    obj *p = new_obj(3);
    p->bytes_per_sec = 48000 * 2 * 2;
    if (src && src->pFormat) {
        pcm_format *f = (pcm_format *)src->pFormat;
        if (f->formatType == 2 || f->formatType == 4) { // PCM or Android PCM_EX
            SLuint32 rate = f->samplesPerSec / 1000; // milliHz
            SLuint32 bytes = f->containerSize ? f->containerSize / 8 : f->bitsPerSample / 8;
            p->bytes_per_sec = rate * f->numChannels * (bytes ? bytes : 2);
        }
    }
    if (!p->bytes_per_sec) p->bytes_per_sec = 192000;
    *out = &p->object;
    return SL_RESULT_SUCCESS;
}
static SLresult Engine_CreateOutputMix(const void *self, const void **out, SLuint32 n, const SLInterfaceID *ids, const SLboolean *req) {
    (void)self; (void)n; (void)ids; (void)req;
    *out = &new_obj(2)->object;
    return SL_RESULT_SUCCESS;
}
static SLresult Engine_QueryNumSupportedInterfaces(const void *self, SLuint32 t, SLuint32 *n) { (void)self; (void)t; if (n) *n = 0; return SL_RESULT_SUCCESS; }
static SLresult Engine_QueryNumSupportedExtensions(const void *self, SLuint32 *n) { (void)self; if (n) *n = 0; return SL_RESULT_SUCCESS; }
static SLresult Engine_IsExtensionSupported(const void *self, const char *name, SLboolean *s) { (void)self; (void)name; if (s) *s = 0; return SL_RESULT_SUCCESS; }
static const void *engine_vtbl[] = {unsupported, unsupported, Engine_CreateAudioPlayer, unsupported, unsupported, unsupported, unsupported,
                                    Engine_CreateOutputMix, unsupported, unsupported, Engine_QueryNumSupportedInterfaces, unsupported,
                                    Engine_QueryNumSupportedExtensions, unsupported, Engine_IsExtensionSupported};

// ---- player thread ----
static uint64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

static void *player_thread(void *arg) {
    obj *p = arg;
    uint64_t next = now_ns();
    pthread_mutex_lock(&p->lock);
    while (!p->quit) {
        if (p->play_state != SL_PLAYSTATE_PLAYING || p->q_count == 0) {
            pthread_cond_wait(&p->cv, &p->lock);
            next = now_ns();
            continue;
        }
        SLuint32 size = p->q[p->q_head].size;
        uint64_t dur = (uint64_t)size * 1000000000ull / p->bytes_per_sec;
        pthread_mutex_unlock(&p->lock);
        // "play" the buffer: wait for its duration in real time
        next += dur;
        uint64_t t = now_ns();
        if (next > t) {
            struct timespec ts = {(time_t)((next - t) / 1000000000ull), (long)((next - t) % 1000000000ull)};
            nanosleep(&ts, NULL);
        } else if (t - next > 200000000ull) {
            next = t; // fell far behind (e.g. debugger); don't burst
        }
        pthread_mutex_lock(&p->lock);
        if (p->q_count == 0) continue; // cleared meanwhile
        p->q_head = (p->q_head + 1) % 64;
        p->q_count--;
        p->played++;
        bq_callback cb = p->cb;
        void *ctx = p->cb_ctx;
        pthread_mutex_unlock(&p->lock);
        if (cb) cb(&p->bq, ctx);
        pthread_mutex_lock(&p->lock);
    }
    pthread_mutex_unlock(&p->lock);
    return NULL;
}

static void ensure_thread(obj *p) {
    if (!p->thread_started) {
        p->thread_started = 1;
        pthread_create(&p->thread, NULL, player_thread, p);
    }
}

// ---- SLPlayItf ----
static SLresult Play_SetPlayState(const void *self, SLuint32 st) {
    obj *p = from(self);
    pthread_mutex_lock(&p->lock);
    p->play_state = st;
    ensure_thread(p);
    pthread_cond_broadcast(&p->cv);
    pthread_mutex_unlock(&p->lock);
    return SL_RESULT_SUCCESS;
}
static SLresult Play_GetPlayState(const void *self, SLuint32 *st) { if (st) *st = from(self)->play_state; return SL_RESULT_SUCCESS; }
static SLresult Play_GetDuration(const void *self, SLuint32 *ms) { (void)self; if (ms) *ms = 0xFFFFFFFF; return SL_RESULT_SUCCESS; }
static SLresult Play_GetPosition(const void *self, SLuint32 *ms) { (void)self; if (ms) *ms = 0; return SL_RESULT_SUCCESS; }
static SLresult Play_ok1(const void *self, SLuint32 a) { (void)self; (void)a; return SL_RESULT_SUCCESS; }
static SLresult Play_ok_ptr(const void *self, void *a) { (void)self; if (a) *(SLuint32 *)a = 0; return SL_RESULT_SUCCESS; }
static SLresult Play_RegisterCallback(const void *self, void *cb, void *ctx) { (void)self; (void)cb; (void)ctx; return SL_RESULT_SUCCESS; }
static SLresult Play_ok0(const void *self) { (void)self; return SL_RESULT_SUCCESS; }
static const void *play_vtbl[] = {Play_SetPlayState, Play_GetPlayState, Play_GetDuration, Play_GetPosition, Play_RegisterCallback, Play_ok1,
                                  Play_ok_ptr, Play_ok1, Play_ok0, Play_ok_ptr, Play_ok1, Play_ok_ptr};

// ---- SLAndroidSimpleBufferQueueItf / SLBufferQueueItf ----
static SLresult BQ_Enqueue(const void *self, const void *buf, SLuint32 size) {
    obj *p = from(self);
    pthread_mutex_lock(&p->lock);
    if (p->q_count >= 64) {
        pthread_mutex_unlock(&p->lock);
        return 8; // SL_RESULT_BUFFER_INSUFFICIENT
    }
    int tail = (p->q_head + p->q_count) % 64;
    p->q[tail].buf = buf;
    p->q[tail].size = size ? size : 1;
    p->q_count++;
    ensure_thread(p);
    pthread_cond_broadcast(&p->cv);
    pthread_mutex_unlock(&p->lock);
    return SL_RESULT_SUCCESS;
}
static SLresult BQ_Clear(const void *self) {
    obj *p = from(self);
    pthread_mutex_lock(&p->lock);
    p->q_count = 0;
    pthread_mutex_unlock(&p->lock);
    return SL_RESULT_SUCCESS;
}
static SLresult BQ_GetState(const void *self, SLuint32 *st) {
    obj *p = from(self);
    if (st) {
        st[0] = (SLuint32)p->q_count;
        st[1] = p->played;
    }
    return SL_RESULT_SUCCESS;
}
static SLresult BQ_RegisterCallback(const void *self, bq_callback cb, void *ctx) {
    obj *p = from(self);
    pthread_mutex_lock(&p->lock);
    p->cb = cb;
    p->cb_ctx = ctx;
    pthread_mutex_unlock(&p->lock);
    return SL_RESULT_SUCCESS;
}
static const void *bq_vtbl[] = {BQ_Enqueue, BQ_Clear, BQ_GetState, BQ_RegisterCallback};

// ---- SLVolumeItf ----
static SLresult Vol_Set(const void *self, SLmillibel l) { (void)self; (void)l; return SL_RESULT_SUCCESS; }
static SLresult Vol_Get(const void *self, SLmillibel *l) { (void)self; if (l) *l = 0; return SL_RESULT_SUCCESS; }
static SLresult Vol_SetB(const void *self, SLboolean b) { (void)self; (void)b; return SL_RESULT_SUCCESS; }
static SLresult Vol_GetB(const void *self, SLboolean *b) { (void)self; if (b) *b = 0; return SL_RESULT_SUCCESS; }
static SLresult Vol_SetP(const void *self, int16_t p) { (void)self; (void)p; return SL_RESULT_SUCCESS; }
static SLresult Vol_GetP(const void *self, int16_t *p) { (void)self; if (p) *p = 0; return SL_RESULT_SUCCESS; }
static const void *vol_vtbl[] = {Vol_Set, Vol_Get, Vol_Get, Vol_SetB, Vol_GetB, Vol_SetB, Vol_GetB, Vol_SetP, Vol_GetP};

// ---- SLAndroidConfigurationItf ----
static SLresult Cfg_Set(const void *self, const char *k, const void *v, SLuint32 n) { (void)self; (void)k; (void)v; (void)n; return SL_RESULT_SUCCESS; }
static SLresult Cfg_Get(const void *self, const char *k, SLuint32 *n, void *v) { (void)self; (void)k; (void)v; if (n) *n = 0; return SL_RESULT_SUCCESS; }
static SLresult Cfg_Proxy(const void *self, void *a) { (void)self; (void)a; return SL_RESULT_FEATURE_UNSUPPORTED; }
static const void *cfg_vtbl[] = {Cfg_Set, Cfg_Get, Cfg_Proxy, Cfg_Proxy};

static obj *new_obj(int kind) {
    obj *o = calloc(1, sizeof *o);
    o->kind = kind;
    o->object = (itf){object_vtbl, o};
    o->engine = (itf){engine_vtbl, o};
    o->play = (itf){play_vtbl, o};
    o->bq = (itf){bq_vtbl, o};
    o->volume = (itf){vol_vtbl, o};
    o->config = (itf){cfg_vtbl, o};
    o->play_state = SL_PLAYSTATE_STOPPED;
    pthread_mutex_init(&o->lock, NULL);
    pthread_cond_init(&o->cv, NULL);
    return o;
}

static SLresult Object_GetInterface(const void *self, SLInterfaceID iid, void *out) {
    obj *o = from(self);
    const itf *r = NULL;
    if (iid == SL_IID_OBJECT) r = &o->object;
    else if (iid == SL_IID_ENGINE && o->kind == 1) r = &o->engine;
    else if (iid == SL_IID_PLAY && o->kind == 3) r = &o->play;
    else if ((iid == SL_IID_ANDROIDSIMPLEBUFFERQUEUE || iid == SL_IID_BUFFERQUEUE) && o->kind == 3) r = &o->bq;
    else if (iid == SL_IID_VOLUME && o->kind == 3) r = &o->volume;
    else if (iid == SL_IID_ANDROIDCONFIGURATION) r = &o->config;
    if (!r) return SL_RESULT_FEATURE_UNSUPPORTED;
    *(const itf **)out = r;
    return SL_RESULT_SUCCESS;
}

static void Object_Destroy(const void *self) {
    obj *o = from(self);
    if (o->thread_started) {
        pthread_mutex_lock(&o->lock);
        o->quit = 1;
        pthread_cond_broadcast(&o->cv);
        pthread_mutex_unlock(&o->lock);
        pthread_join(o->thread, NULL);
    }
    free(o);
}

SLresult slCreateEngine(const void **engine, SLuint32 n, const void *opts, SLuint32 ni, const SLInterfaceID *ids, const SLboolean *req) {
    (void)n; (void)opts; (void)ni; (void)ids; (void)req;
    *engine = &new_obj(1)->object;
    wwd_host_log(4, "wwd-audio", "OpenSL ES engine created (null sink)");
    return SL_RESULT_SUCCESS;
}

SLresult slQueryNumSupportedEngineInterfaces(SLuint32 *n) { if (n) *n = 0; return SL_RESULT_SUCCESS; }
SLresult slQuerySupportedEngineInterfaces(SLuint32 i, SLInterfaceID *id) { (void)i; (void)id; return SL_RESULT_PARAMETER_INVALID; }
