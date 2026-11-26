// liblog replacement: Android log calls go to the host's stderr.
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "wwd_host.h"

static int min_prio = -1;

static int threshold(void) {
    if (min_prio < 0) {
        const char *e = getenv("WWD_LOG");
        min_prio = e ? atoi(e) : 4; // 2 verbose .. 4 info .. 6 error
    }
    return min_prio;
}

int __android_log_write(int prio, const char *tag, const char *text) {
    if (prio < threshold()) return 0;
    wwd_host_log(prio, tag ? tag : "", text ? text : "");
    return 1;
}

int __android_log_vprint(int prio, const char *tag, const char *fmt, va_list ap) {
    if (prio < threshold()) return 0;
    char buf[4096];
    vsnprintf(buf, sizeof buf, fmt, ap);
    return __android_log_write(prio, tag, buf);
}

int __android_log_print(int prio, const char *tag, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = __android_log_vprint(prio, tag, fmt, ap);
    va_end(ap);
    return r;
}

int __android_log_buf_write(int buf, int prio, const char *tag, const char *text) { (void)buf; return __android_log_write(prio, tag, text); }

int __android_log_buf_print(int buf, int prio, const char *tag, const char *fmt, ...) {
    (void)buf;
    va_list ap;
    va_start(ap, fmt);
    int r = __android_log_vprint(prio, tag, fmt, ap);
    va_end(ap);
    return r;
}

void __android_log_assert(const char *cond, const char *tag, const char *fmt, ...) {
    char buf[2048] = "assertion failed";
    if (fmt) {
        va_list ap;
        va_start(ap, fmt);
        vsnprintf(buf, sizeof buf, fmt, ap);
        va_end(ap);
    }
    wwd_host_log(7, tag ? tag : "", buf);
    if (cond) wwd_host_log(7, tag ? tag : "", cond);
    abort();
}

int __android_log_is_loggable(int prio, const char *tag, int def) { (void)tag; (void)def; return prio >= threshold(); }
int __android_log_is_loggable_len(int prio, const char *tag, size_t len, int def) { (void)len; return __android_log_is_loggable(prio, tag, def); }
int __android_log_set_minimum_priority(int prio) { int o = min_prio; min_prio = prio; return o; }
int __android_log_get_minimum_priority(void) { return threshold(); }
void __android_log_set_logger(void *l) { (void)l; }
void __android_log_set_aborter(void *a) { (void)a; }
void __android_log_set_default_tag(const char *t) { (void)t; }
int __android_log_error_write(int tag, const char *sub, int uid, const char *data, unsigned len) { (void)tag; (void)sub; (void)uid; (void)data; (void)len; return 0; }

struct __android_log_message {
    size_t struct_size;
    int buffer_id, priority;
    const char *tag, *file;
    unsigned line;
    const char *message;
};
void __android_log_write_log_message(struct __android_log_message *m) {
    if (m) __android_log_write(m->priority, m->tag, m->message);
}
void __android_log_logd_logger(const struct __android_log_message *m) { __android_log_write_log_message((struct __android_log_message *)m); }
void __android_log_stderr_logger(const struct __android_log_message *m) { __android_log_write_log_message((struct __android_log_message *)m); }
void __android_log_default_aborter(const char *msg) { wwd_host_log(7, "abort", msg); abort(); }
void __android_log_call_aborter(const char *msg) { __android_log_default_aborter(msg); }
