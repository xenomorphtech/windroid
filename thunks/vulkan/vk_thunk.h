// Guest-side Vulkan thunk support (compiled for aarch64-linux).
#pragma once

#define WWD_SVC_THUNK "0x5757"

// Every thunked command is a naked stub: put the command id in x8 and trap.
// All argument registers and the stack are left untouched for the host.
#define WWD_THUNK(name, id)                                                   \
    __attribute__((naked, visibility("default"))) void name(void) {           \
        __asm__ volatile("mov x8, #" #id "\n\tsvc #" WWD_SVC_THUNK "\n\tret"); \
    }

struct wwd_proc {
    const char *name;
    void *fn;
};

extern const struct wwd_proc wwd_vk_procs[];
extern const unsigned wwd_vk_nprocs;

// implemented in vk_loader.c
void (*vkGetInstanceProcAddr(void *instance, const char *name))(void);
void (*vkGetDeviceProcAddr(void *device, const char *name))(void);
