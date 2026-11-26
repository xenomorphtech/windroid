// Guest side of the wwd host-service hypercall (svc #0x5758).
// Must match crates/wwd-linux/src/thunk/host.rs and window.rs.
#pragma once
#include <stdint.h>

static inline uint64_t wwd_hostcall(uint64_t id, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3) {
    register uint64_t x0 __asm__("x0") = a0;
    register uint64_t x1 __asm__("x1") = a1;
    register uint64_t x2 __asm__("x2") = a2;
    register uint64_t x3 __asm__("x3") = a3;
    register uint64_t x8 __asm__("x8") = id;
    __asm__ volatile("svc #0x5758" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x8) : "memory");
    return x0;
}

typedef struct {
    uint32_t kind; // 1 down, 2 up, 3 move, 4 key down, 5 key up, 6 resize, 7 close, 8 focus+, 9 focus-
    uint32_t key;
    float x, y;
    uint32_t w, h;
} wwd_event;

#define WWD_WINDOW_MAGIC 0x57445757u

// Our ANativeWindow. The host reads magic and host_window (vulkan.rs).
struct ANativeWindow {
    uint32_t magic;
    uint32_t refs;
    uint64_t host_window;
    int32_t width, height, format;
};

static inline uint64_t wwd_host_window_create(uint32_t w, uint32_t h) { return wwd_hostcall(1, w, h, 0, 0); }
static inline void wwd_host_window_size(uint64_t id, uint32_t *w, uint32_t *h) { wwd_hostcall(2, id, (uint64_t)w, (uint64_t)h, 0); }
static inline int wwd_host_window_poll(uint64_t id, wwd_event *e) { return (int)wwd_hostcall(3, id, (uint64_t)e, 0, 0); }
static inline void wwd_host_log(int prio, const char *tag, const char *msg) { wwd_hostcall(5, (uint64_t)prio, (uint64_t)tag, (uint64_t)msg, 0); }
static inline uint64_t wwd_host_monotonic_ns(void) { return wwd_hostcall(6, 0, 0, 0, 0); }
