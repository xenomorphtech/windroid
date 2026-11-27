// Replacement for Android's libaaudio.so: real-time paced null output.
//
// The system libaaudio talks to audioserver over binder, which doesn't exist
// here, so streams never run. Engines that clock themselves from the audio
// device (Wwise) then stall. These streams consume audio at the requested
// sample rate: the data callback runs on its own thread every burst, blocking
// writes are paced, and timestamps advance in real time. Input streams
// produce silence.
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <stdio.h>
#include "wwd_host.h"

#define ALOG(...) do { char m_[256]; snprintf(m_, sizeof m_, __VA_ARGS__); wwd_host_log(4, "wwd-audio", m_); } while (0)

#define EXPORT __attribute__((visibility("default")))

typedef int32_t aaudio_result_t;
enum { AAUDIO_OK = 0, AAUDIO_ERROR_ILLEGAL_ARGUMENT = -898, AAUDIO_ERROR_INVALID_STATE = -895, AAUDIO_ERROR_TIMEOUT = -885 };
enum {
    AAUDIO_STREAM_STATE_UNINITIALIZED = 0, AAUDIO_STREAM_STATE_OPEN = 2, AAUDIO_STREAM_STATE_STARTING = 3,
    AAUDIO_STREAM_STATE_STARTED = 4, AAUDIO_STREAM_STATE_PAUSING = 5, AAUDIO_STREAM_STATE_PAUSED = 6,
    AAUDIO_STREAM_STATE_FLUSHING = 7, AAUDIO_STREAM_STATE_FLUSHED = 8, AAUDIO_STREAM_STATE_STOPPING = 9,
    AAUDIO_STREAM_STATE_STOPPED = 10, AAUDIO_STREAM_STATE_CLOSING = 11, AAUDIO_STREAM_STATE_CLOSED = 12,
};
enum { AAUDIO_FORMAT_PCM_I16 = 1, AAUDIO_FORMAT_PCM_FLOAT = 2, AAUDIO_FORMAT_PCM_I24_PACKED = 3, AAUDIO_FORMAT_PCM_I32 = 4 };
enum { AAUDIO_DIRECTION_OUTPUT = 0, AAUDIO_DIRECTION_INPUT = 1 };
enum { AAUDIO_CALLBACK_RESULT_CONTINUE = 0, AAUDIO_CALLBACK_RESULT_STOP = 1 };

typedef struct AAudioStreamStruct AAudioStream;
typedef int32_t (*AAudioStream_dataCallback)(AAudioStream *, void *, void *, int32_t);
typedef void (*AAudioStream_errorCallback)(AAudioStream *, void *, aaudio_result_t);

typedef struct {
    int32_t device, rate, channels, format, sharing, direction, capacity, perf, usage, content, preset, session, frames_per_cb;
    int32_t channel_mask, spatialization, spatialized;
    AAudioStream_dataCallback data_cb;
    void *data_user;
    AAudioStream_errorCallback err_cb;
    void *err_user;
} AAudioStreamBuilder;

struct AAudioStreamStruct {
    AAudioStreamBuilder cfg;
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int32_t state;
    int64_t frames;         // frames consumed (output) / produced (input)
    int64_t start_ns;       // monotonic time when `start_frames` was the position
    int64_t start_frames;
    int32_t burst, buffer_size;
    pthread_t thread;
    int thread_running;
    void *cb_buf;
};

static int64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000000000LL + t.tv_nsec;
}

static int bytes_per_frame(const AAudioStream *s) {
    int b = s->cfg.format == AAUDIO_FORMAT_PCM_I16 ? 2 : s->cfg.format == AAUDIO_FORMAT_PCM_I24_PACKED ? 3 : 4;
    return b * (s->cfg.channels > 0 ? s->cfg.channels : 2);
}

// frames the device "should" have consumed by now while started
static int64_t position_now(AAudioStream *s) {
    if (s->state != AAUDIO_STREAM_STATE_STARTED) return s->frames;
    return s->start_frames + (now_ns() - s->start_ns) * s->cfg.rate / 1000000000LL;
}

static void set_state(AAudioStream *s, int32_t st) {
    s->state = st;
    pthread_cond_broadcast(&s->cv);
}

static void *callback_thread(void *arg) {
    AAudioStream *s = arg;
    int64_t period = (int64_t)s->burst * 1000000000LL / s->cfg.rate;
    struct timespec next;
    clock_gettime(CLOCK_MONOTONIC, &next);
    for (;;) {
        pthread_mutex_lock(&s->mu);
        int st = s->state;
        pthread_mutex_unlock(&s->mu);
        if (st != AAUDIO_STREAM_STATE_STARTED) break;
        if (s->cfg.direction == AAUDIO_DIRECTION_INPUT) memset(s->cb_buf, 0, (size_t)s->burst * bytes_per_frame(s));
        int r = s->cfg.data_cb(s, s->cfg.data_user, s->cb_buf, s->burst);
        pthread_mutex_lock(&s->mu);
        s->frames += s->burst;
        int64_t done = s->frames;
        pthread_mutex_unlock(&s->mu);
        if (done == s->burst || done % ((int64_t)s->cfg.rate * 30 / s->burst * s->burst) == 0)
            ALOG("AAudio callback running: %lld frames (%lld s), last result %d", (long long)done, (long long)(done / s->cfg.rate), r);
        if (r == AAUDIO_CALLBACK_RESULT_STOP) {
            pthread_mutex_lock(&s->mu);
            set_state(s, AAUDIO_STREAM_STATE_STOPPED);
            pthread_mutex_unlock(&s->mu);
            break;
        }
        next.tv_nsec += period;
        while (next.tv_nsec >= 1000000000L) {
            next.tv_nsec -= 1000000000L;
            next.tv_sec++;
        }
        clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, NULL);
    }
    return NULL;
}

// ---- builder ----
EXPORT aaudio_result_t AAudio_createStreamBuilder(AAudioStreamBuilder **out) {
    AAudioStreamBuilder *b = calloc(1, sizeof *b);
    b->format = AAUDIO_FORMAT_PCM_FLOAT;
    b->session = -1;
    *out = b;
    ALOG("AAudio builder created");
    return AAUDIO_OK;
}
EXPORT aaudio_result_t AAudioStreamBuilder_delete(AAudioStreamBuilder *b) { free(b); return AAUDIO_OK; }
EXPORT void AAudioStreamBuilder_setDeviceId(AAudioStreamBuilder *b, int32_t v) { b->device = v; }
EXPORT void AAudioStreamBuilder_setSampleRate(AAudioStreamBuilder *b, int32_t v) { b->rate = v; }
EXPORT void AAudioStreamBuilder_setChannelCount(AAudioStreamBuilder *b, int32_t v) { b->channels = v; }
EXPORT void AAudioStreamBuilder_setSamplesPerFrame(AAudioStreamBuilder *b, int32_t v) { b->channels = v; }
EXPORT void AAudioStreamBuilder_setChannelMask(AAudioStreamBuilder *b, uint32_t v) {
    b->channel_mask = (int32_t)v;
    if (v) b->channels = __builtin_popcount(v);
}
EXPORT void AAudioStreamBuilder_setFormat(AAudioStreamBuilder *b, int32_t v) { b->format = v; }
EXPORT void AAudioStreamBuilder_setSharingMode(AAudioStreamBuilder *b, int32_t v) { b->sharing = v; }
EXPORT void AAudioStreamBuilder_setDirection(AAudioStreamBuilder *b, int32_t v) { b->direction = v; }
EXPORT void AAudioStreamBuilder_setBufferCapacityInFrames(AAudioStreamBuilder *b, int32_t v) { b->capacity = v; }
EXPORT void AAudioStreamBuilder_setPerformanceMode(AAudioStreamBuilder *b, int32_t v) { b->perf = v; }
EXPORT void AAudioStreamBuilder_setUsage(AAudioStreamBuilder *b, int32_t v) { b->usage = v; }
EXPORT void AAudioStreamBuilder_setContentType(AAudioStreamBuilder *b, int32_t v) { b->content = v; }
EXPORT void AAudioStreamBuilder_setInputPreset(AAudioStreamBuilder *b, int32_t v) { b->preset = v; }
EXPORT void AAudioStreamBuilder_setSessionId(AAudioStreamBuilder *b, int32_t v) { b->session = v; }
EXPORT void AAudioStreamBuilder_setFramesPerDataCallback(AAudioStreamBuilder *b, int32_t v) { b->frames_per_cb = v; }
EXPORT void AAudioStreamBuilder_setSpatializationBehavior(AAudioStreamBuilder *b, int32_t v) { b->spatialization = v; }
EXPORT void AAudioStreamBuilder_setIsContentSpatialized(AAudioStreamBuilder *b, int v) { b->spatialized = v; }
EXPORT void AAudioStreamBuilder_setAllowedCapturePolicy(AAudioStreamBuilder *b, int32_t v) { (void)b; (void)v; }
EXPORT void AAudioStreamBuilder_setPrivacySensitive(AAudioStreamBuilder *b, int v) { (void)b; (void)v; }
EXPORT void AAudioStreamBuilder_setDataCallback(AAudioStreamBuilder *b, AAudioStream_dataCallback cb, void *user) {
    b->data_cb = cb;
    b->data_user = user;
}
EXPORT void AAudioStreamBuilder_setErrorCallback(AAudioStreamBuilder *b, AAudioStream_errorCallback cb, void *user) {
    b->err_cb = cb;
    b->err_user = user;
}

EXPORT aaudio_result_t AAudioStreamBuilder_openStream(AAudioStreamBuilder *b, AAudioStream **out) {
    AAudioStream *s = calloc(1, sizeof *s);
    s->cfg = *b;
    if (s->cfg.rate <= 0) s->cfg.rate = 48000;
    if (s->cfg.channels <= 0) s->cfg.channels = 2;
    if (s->cfg.format <= 0) s->cfg.format = AAUDIO_FORMAT_PCM_FLOAT;
    if (s->cfg.device <= 0) s->cfg.device = 2;
    if (s->cfg.session < 0) s->cfg.session = 0;
    s->burst = s->cfg.frames_per_cb > 0 ? s->cfg.frames_per_cb : 256;
    if (s->cfg.capacity <= 0) s->cfg.capacity = s->burst * 8;
    s->buffer_size = s->burst * 2;
    pthread_mutex_init(&s->mu, NULL);
    pthread_cond_init(&s->cv, NULL);
    s->state = AAUDIO_STREAM_STATE_OPEN;
    s->cb_buf = calloc((size_t)s->burst, (size_t)bytes_per_frame(s));
    *out = s;
    ALOG("AAudio openStream: dir %d rate %d ch %d (mask %#x) fmt %d sharing %d perf %d usage %d cbframes %d cap %d datacb %d",
         b->direction, b->rate, b->channels, b->channel_mask, b->format, b->sharing, b->perf, b->usage, b->frames_per_cb,
         b->capacity, b->data_cb != NULL);
    return AAUDIO_OK;
}

// ---- stream control ----
EXPORT aaudio_result_t AAudioStream_requestStart(AAudioStream *s) {
    ALOG("AAudio requestStart (state %d)", s->state);
    pthread_mutex_lock(&s->mu);
    s->start_ns = now_ns();
    s->start_frames = s->frames;
    set_state(s, AAUDIO_STREAM_STATE_STARTED);
    int need_thread = s->cfg.data_cb && !s->thread_running;
    if (need_thread) s->thread_running = 1;
    pthread_mutex_unlock(&s->mu);
    if (need_thread) pthread_create(&s->thread, NULL, callback_thread, s);
    return AAUDIO_OK;
}

static aaudio_result_t halt(AAudioStream *s, int32_t final_state) {
    ALOG("AAudio stream -> state %d (consumed %lld frames)", final_state, (long long)s->frames);
    pthread_mutex_lock(&s->mu);
    s->frames = position_now(s) > s->frames && !s->cfg.data_cb ? position_now(s) : s->frames;
    set_state(s, final_state);
    int join = s->thread_running;
    s->thread_running = 0;
    pthread_mutex_unlock(&s->mu);
    if (join && !pthread_equal(pthread_self(), s->thread)) pthread_join(s->thread, NULL);
    return AAUDIO_OK;
}
EXPORT aaudio_result_t AAudioStream_requestPause(AAudioStream *s) { return halt(s, AAUDIO_STREAM_STATE_PAUSED); }
EXPORT aaudio_result_t AAudioStream_requestStop(AAudioStream *s) { return halt(s, AAUDIO_STREAM_STATE_STOPPED); }
EXPORT aaudio_result_t AAudioStream_requestFlush(AAudioStream *s) {
    pthread_mutex_lock(&s->mu);
    set_state(s, AAUDIO_STREAM_STATE_FLUSHED);
    pthread_mutex_unlock(&s->mu);
    return AAUDIO_OK;
}
EXPORT aaudio_result_t AAudioStream_close(AAudioStream *s) {
    halt(s, AAUDIO_STREAM_STATE_CLOSED);
    free(s->cb_buf);
    free(s);
    return AAUDIO_OK;
}
EXPORT int32_t AAudioStream_getState(AAudioStream *s) { return s->state; }
EXPORT aaudio_result_t AAudioStream_waitForStateChange(AAudioStream *s, int32_t input, int32_t *next, int64_t timeout_ns) {
    struct timespec dl;
    clock_gettime(CLOCK_REALTIME, &dl);
    int64_t t = (int64_t)dl.tv_nsec + timeout_ns;
    dl.tv_sec += t / 1000000000LL;
    dl.tv_nsec = t % 1000000000LL;
    pthread_mutex_lock(&s->mu);
    while (s->state == input) {
        if (pthread_cond_timedwait(&s->cv, &s->mu, &dl) == ETIMEDOUT) break;
    }
    int32_t st = s->state;
    pthread_mutex_unlock(&s->mu);
    if (next) *next = st;
    return st == input ? AAUDIO_ERROR_TIMEOUT : AAUDIO_OK;
}

// blocking I/O, paced to the sample rate
static int32_t transfer(AAudioStream *s, int32_t frames, int64_t timeout_ns) {
    if (s->state != AAUDIO_STREAM_STATE_STARTED) return AAUDIO_ERROR_INVALID_STATE;
    pthread_mutex_lock(&s->mu);
    int64_t pos = position_now(s);
    // room = frames the device has consumed beyond what was written, plus one buffer
    int64_t room = pos + s->buffer_size - s->frames;
    if (room < frames && timeout_ns > 0) {
        int64_t wait = (frames - room) * 1000000000LL / s->cfg.rate;
        if (wait > timeout_ns) wait = timeout_ns;
        pthread_mutex_unlock(&s->mu);
        struct timespec ts = {(time_t)(wait / 1000000000LL), (long)(wait % 1000000000LL)};
        nanosleep(&ts, NULL);
        pthread_mutex_lock(&s->mu);
        room = position_now(s) + s->buffer_size - s->frames;
    }
    int32_t n = room <= 0 ? 0 : room < frames ? (int32_t)room : frames;
    s->frames += n;
    pthread_mutex_unlock(&s->mu);
    return n;
}
EXPORT aaudio_result_t AAudioStream_write(AAudioStream *s, const void *buf, int32_t frames, int64_t timeout_ns) {
    (void)buf;
    return transfer(s, frames, timeout_ns);
}
EXPORT aaudio_result_t AAudioStream_read(AAudioStream *s, void *buf, int32_t frames, int64_t timeout_ns) {
    int32_t n = transfer(s, frames, timeout_ns);
    if (n > 0) memset(buf, 0, (size_t)n * bytes_per_frame(s));
    return n;
}

// ---- properties ----
EXPORT int32_t AAudioStream_getSampleRate(AAudioStream *s) { return s->cfg.rate; }
EXPORT int32_t AAudioStream_getChannelCount(AAudioStream *s) { return s->cfg.channels; }
EXPORT int32_t AAudioStream_getSamplesPerFrame(AAudioStream *s) { return s->cfg.channels; }
EXPORT uint32_t AAudioStream_getChannelMask(AAudioStream *s) { return (uint32_t)s->cfg.channel_mask; }
EXPORT int32_t AAudioStream_getFormat(AAudioStream *s) { return s->cfg.format; }
EXPORT int32_t AAudioStream_getDeviceId(AAudioStream *s) { return s->cfg.device; }
EXPORT int32_t AAudioStream_getSharingMode(AAudioStream *s) { return s->cfg.sharing; }
EXPORT int32_t AAudioStream_getPerformanceMode(AAudioStream *s) { return s->cfg.perf; }
EXPORT int32_t AAudioStream_getDirection(AAudioStream *s) { return s->cfg.direction; }
EXPORT int32_t AAudioStream_getUsage(AAudioStream *s) { return s->cfg.usage; }
EXPORT int32_t AAudioStream_getContentType(AAudioStream *s) { return s->cfg.content; }
EXPORT int32_t AAudioStream_getInputPreset(AAudioStream *s) { return s->cfg.preset; }
EXPORT int32_t AAudioStream_getSessionId(AAudioStream *s) { return s->cfg.session; }
EXPORT int32_t AAudioStream_getSpatializationBehavior(AAudioStream *s) { return s->cfg.spatialization ? s->cfg.spatialization : 1; }
EXPORT int AAudioStream_isContentSpatialized(AAudioStream *s) { return s->cfg.spatialized; }
EXPORT int AAudioStream_isMMapUsed(AAudioStream *s) { (void)s; return 0; }
EXPORT int32_t AAudioStream_getFramesPerBurst(AAudioStream *s) { return s->burst; }
EXPORT int32_t AAudioStream_getFramesPerDataCallback(AAudioStream *s) { return s->cfg.frames_per_cb; }
EXPORT int32_t AAudioStream_getBufferCapacityInFrames(AAudioStream *s) { return s->cfg.capacity; }
EXPORT int32_t AAudioStream_getBufferSizeInFrames(AAudioStream *s) { return s->buffer_size; }
EXPORT aaudio_result_t AAudioStream_setBufferSizeInFrames(AAudioStream *s, int32_t n) {
    if (n <= 0) n = s->burst;
    if (n > s->cfg.capacity) n = s->cfg.capacity;
    s->buffer_size = n;
    return n;
}
EXPORT int32_t AAudioStream_getXRunCount(AAudioStream *s) { (void)s; return 0; }
EXPORT int64_t AAudioStream_getFramesWritten(AAudioStream *s) {
    pthread_mutex_lock(&s->mu);
    int64_t v = s->cfg.direction == AAUDIO_DIRECTION_OUTPUT ? s->frames : position_now(s);
    pthread_mutex_unlock(&s->mu);
    return v;
}
EXPORT int64_t AAudioStream_getFramesRead(AAudioStream *s) {
    pthread_mutex_lock(&s->mu);
    int64_t v = s->cfg.data_cb ? s->frames : (position_now(s) < s->frames ? position_now(s) : s->frames);
    pthread_mutex_unlock(&s->mu);
    return v;
}
EXPORT aaudio_result_t AAudioStream_getTimestamp(AAudioStream *s, int32_t clockid, int64_t *frame_pos, int64_t *time_ns) {
    (void)clockid;
    if (s->state != AAUDIO_STREAM_STATE_STARTED) return AAUDIO_ERROR_INVALID_STATE;
    pthread_mutex_lock(&s->mu);
    *frame_pos = s->cfg.data_cb ? s->frames : position_now(s);
    *time_ns = now_ns();
    pthread_mutex_unlock(&s->mu);
    return AAUDIO_OK;
}

EXPORT const char *AAudio_convertResultToText(aaudio_result_t r) { return r == AAUDIO_OK ? "AAUDIO_OK" : "AAUDIO_ERROR"; }
EXPORT const char *AAudio_convertStreamStateToText(int32_t s) { (void)s; return "AAUDIO_STREAM_STATE"; }
