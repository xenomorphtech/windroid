// Guest-side replacement for libvulkan.so.1: GetProcAddr is implemented here
// (returning guest stubs), everything else is a thunk to the host's vulkan-1.dll.
#include <stddef.h>
#include <string.h>
#include "vk_thunk.h"

typedef void (*PFN_vkVoidFunction)(void);

// id 0: ask the host whether `name` resolves for `handle` (kind 0 = instance, 1 = device)
__attribute__((naked)) static int wwd_vk_has(void *handle, const char *name, int kind) {
    __asm__ volatile("mov x8, #0\n\tsvc #" WWD_SVC_THUNK "\n\tret");
}

static void *lookup(const char *name) {
    unsigned lo = 0, hi = wwd_vk_nprocs;
    while (lo < hi) {
        unsigned mid = (lo + hi) / 2;
        int c = strcmp(name, wwd_vk_procs[mid].name);
        if (c == 0) return wwd_vk_procs[mid].fn;
        if (c < 0) hi = mid; else lo = mid + 1;
    }
    return NULL;
}

__attribute__((visibility("default"))) PFN_vkVoidFunction vkGetInstanceProcAddr(void *instance, const char *name) {
    if (!name) return NULL;
    void *f = lookup(name);
    if (!f) {
        wwd_vk_has(instance, name, 2); // lets the host log unknown names
        return NULL;
    }
    if (!strcmp(name, "vkGetInstanceProcAddr") || !strcmp(name, "vkGetDeviceProcAddr")) return (PFN_vkVoidFunction)f;
    return wwd_vk_has(instance, name, 0) ? (PFN_vkVoidFunction)f : NULL;
}

__attribute__((visibility("default"))) PFN_vkVoidFunction vkGetDeviceProcAddr(void *device, const char *name) {
    if (!name) return NULL;
    void *f = lookup(name);
    if (!f) {
        wwd_vk_has(device, name, 2);
        return NULL;
    }
    if (!strcmp(name, "vkGetDeviceProcAddr")) return (PFN_vkVoidFunction)f;
    return wwd_vk_has(device, name, 1) ? (PFN_vkVoidFunction)f : NULL;
}

// Loader/ICD interface entry points some apps probe for.
__attribute__((visibility("default"))) PFN_vkVoidFunction vk_icdGetInstanceProcAddr(void *instance, const char *name) {
    return vkGetInstanceProcAddr(instance, name);
}
