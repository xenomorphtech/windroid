// libswappy replacement (Android Game SDK frame pacing).
// Real Swappy paces presents with AChoreographer and a Java helper class;
// under wwd presents go straight to the host Vulkan driver, which already
// paces to the host display. SwappyVk_queuePresent is a plain vkQueuePresentKHR.
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

typedef void *VkQueue, *VkPhysicalDevice, *VkDevice;
typedef uint64_t VkSwapchainKHR;
typedef int32_t VkResult;
struct VkPresentInfoKHR;
struct VkExtensionProperties;

extern VkResult vkQueuePresentKHR(VkQueue queue, const struct VkPresentInfoKHR *info);

#define REFRESH_NS 16666667ull

// ---- Vulkan ----
void SwappyVk_determineDeviceExtensions(VkPhysicalDevice pd, uint32_t n, struct VkExtensionProperties *avail, uint32_t *req_count, char **req) {
    (void)pd; (void)n; (void)avail; (void)req;
    if (req_count) *req_count = 0;
}
void SwappyVk_setQueueFamilyIndex(VkDevice d, VkQueue q, uint32_t idx) { (void)d; (void)q; (void)idx; }
bool SwappyVk_initAndGetRefreshCycleDuration(void *env, void *activity, VkPhysicalDevice pd, VkDevice d, VkSwapchainKHR sc, uint64_t *refresh) {
    (void)env; (void)activity; (void)pd; (void)d; (void)sc;
    if (refresh) *refresh = REFRESH_NS;
    return true;
}
void SwappyVk_setWindow(VkDevice d, VkSwapchainKHR sc, void *window) { (void)d; (void)sc; (void)window; }
void SwappyVk_setSwapIntervalNS(VkDevice d, VkSwapchainKHR sc, uint64_t ns) { (void)d; (void)sc; (void)ns; }
VkResult SwappyVk_queuePresent(VkQueue q, const struct VkPresentInfoKHR *info) { return vkQueuePresentKHR(q, info); }
void SwappyVk_destroySwapchain(VkDevice d, VkSwapchainKHR sc) { (void)d; (void)sc; }
void SwappyVk_destroyDevice(VkDevice d) { (void)d; }
void SwappyVk_setAutoSwapInterval(bool e) { (void)e; }
void SwappyVk_setAutoPipelineMode(bool e) { (void)e; }
void SwappyVk_setMaxAutoSwapIntervalNS(uint64_t ns) { (void)ns; }
void SwappyVk_enableStats(VkSwapchainKHR sc, bool e) { (void)sc; (void)e; }
void SwappyVk_recordFrameStart(VkQueue q, VkSwapchainKHR sc, uint32_t img) { (void)q; (void)sc; (void)img; }
void SwappyVk_getStats(VkSwapchainKHR sc, void *stats) { (void)sc; if (stats) memset(stats, 0, 4 * 8 * 6); }
void SwappyVk_clearStats(VkSwapchainKHR sc) { (void)sc; }
void SwappyVk_injectTracer(const void *t) { (void)t; }
void SwappyVk_uninjectTracer(const void *t) { (void)t; }
void SwappyVk_setFunctionProvider(const void *p) { (void)p; }
bool SwappyVk_isEnabled(VkSwapchainKHR sc, bool *enabled) {
    (void)sc;
    if (enabled) *enabled = false;
    return true;
}
uint64_t SwappyVk_getSwapIntervalNS(VkSwapchainKHR sc) { (void)sc; return REFRESH_NS; }
uint64_t SwappyVk_getFenceTimeoutNS(void) { return 50000000ull; }
void SwappyVk_setFenceTimeoutNS(uint64_t ns) { (void)ns; }
void SwappyVk_enableFramePacing(VkSwapchainKHR sc, bool e) { (void)sc; (void)e; }
void SwappyVk_enableBlockingWait(VkSwapchainKHR sc, bool e) { (void)sc; (void)e; }
void SwappyVk_resetFramePacing(VkSwapchainKHR sc) { (void)sc; }
int SwappyVk_getSupportedRefreshPeriodsNS(uint64_t *out, int size, VkSwapchainKHR sc) {
    (void)sc;
    if (out && size > 0) out[0] = REFRESH_NS;
    return 1;
}

// ---- GL: never initializes (rendering is Vulkan) ----
bool SwappyGL_init(void *env, void *activity) { (void)env; (void)activity; return false; }
void SwappyGL_destroy(void) {}
bool SwappyGL_isEnabled(void) { return false; }
bool SwappyGL_swap(void *dpy, void *surf) { (void)dpy; (void)surf; return false; }
bool SwappyGL_setWindow(void *w) { (void)w; return false; }
void SwappyGL_setSwapIntervalNS(uint64_t ns) { (void)ns; }
uint64_t SwappyGL_getSwapIntervalNS(void) { return REFRESH_NS; }
uint64_t SwappyGL_getRefreshPeriodNanos(void) { return REFRESH_NS; }
void SwappyGL_setAutoSwapInterval(bool e) { (void)e; }
void SwappyGL_setAutoPipelineMode(bool e) { (void)e; }
void SwappyGL_setMaxAutoSwapIntervalNS(uint64_t ns) { (void)ns; }
void SwappyGL_enableStats(bool e) { (void)e; }
void SwappyGL_recordFrameStart(void *dpy, void *surf) { (void)dpy; (void)surf; }
void SwappyGL_getStats(void *s) { (void)s; }
void SwappyGL_clearStats(void) {}
void SwappyGL_injectTracer(const void *t) { (void)t; }
void SwappyGL_uninjectTracer(const void *t) { (void)t; }
void SwappyGL_setFenceTimeoutNS(uint64_t ns) { (void)ns; }
uint64_t SwappyGL_getFenceTimeoutNS(void) { return 50000000ull; }
void SwappyGL_setUseAffinity(bool e) { (void)e; }
bool SwappyGL_getUseAffinity(void) { return false; }
void SwappyGL_onChoreographer(int64_t t) { (void)t; }
void SwappyGL_enableFramePacing(bool e) { (void)e; }
void SwappyGL_enableBlockingWait(bool e) { (void)e; }
void SwappyGL_resetFramePacing(void) {}
void SwappyGL_setBufferStuffingFixWait(int32_t n) { (void)n; }
int SwappyGL_getSupportedRefreshPeriodsNS(uint64_t *out, int size) {
    if (out && size > 0) out[0] = REFRESH_NS;
    return 1;
}

uint32_t Swappy_version(void) { return (1u << 16) | (10u << 8); }
const char *Swappy_versionString(void) { return "1.10.0-wwd"; }
void Swappy_setThreadFunctions(const void *f) { (void)f; }
