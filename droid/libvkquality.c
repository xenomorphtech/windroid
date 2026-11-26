// libvkquality replacement (Android Game SDK "VK Quality").
// The real library fingerprints the GPU through EGL/GLES and a device list;
// under wwd the GPU is a desktop part with a full Vulkan driver, so always
// recommend Vulkan.
#include <stdint.h>
#include <stdlib.h>
#include "wwd_host.h"

// enum vkQualityRecommendation (vkquality.h)
#define kRecommendationVulkanBecauseDeviceMatch 2

static int g_init;

static int pick(void) {
    const char *e = getenv("WWD_VKQUALITY");
    return e ? atoi(e) : kRecommendationVulkanBecauseDeviceMatch;
}

int vkQuality_initialize(void *env, void *asset_manager, const char *storage, const char *asset) {
    (void)env; (void)asset_manager; (void)storage; (void)asset;
    g_init = 1;
    return 0; // kSuccess
}
int vkQuality_initializeFlags(void *env, void *am, const char *s, const char *a, uint32_t flags) {
    (void)flags;
    return vkQuality_initialize(env, am, s, a);
}
int vkQuality_initializeFlagsInfo(void *env, void *am, const char *s, const char *a, uint32_t flags, void *info) {
    (void)flags; (void)info;
    return vkQuality_initialize(env, am, s, a);
}
int vkQuality_getRecommendation(void) {
    int r = pick();
    wwd_host_log(4, "wwd-vkquality", r == kRecommendationVulkanBecauseDeviceMatch ? "recommending Vulkan" : "recommendation overridden");
    return g_init ? r : 0;
}
void vkQuality_destroy(void) { g_init = 0; }
