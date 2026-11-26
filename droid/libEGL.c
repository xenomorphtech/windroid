// libEGL for wwd: a query-only EGL. Rendering goes through Vulkan (thunked to
// the host GPU); this exists so engines' device detection (GPU family, GLES
// version, compressed texture support) sees a modern Vulkan-class mobile GPU.
// Displays, configs, contexts and surfaces are opaque handles that always work.
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>
#include "wwd_host.h"

typedef void *EGLDisplay, *EGLConfig, *EGLContext, *EGLSurface, *EGLClientBuffer, *EGLImageKHR;
typedef int32_t EGLint;
typedef unsigned EGLBoolean, EGLenum;

#define EGL_SUCCESS 0x3000
#define EGL_VENDOR 0x3053
#define EGL_VERSION 0x3054
#define EGL_EXTENSIONS 0x3055
#define EGL_CLIENT_APIS 0x308D
#define EGL_WIDTH 0x3057
#define EGL_HEIGHT 0x3056

static EGLDisplay g_cur_dpy;
static EGLContext g_cur_ctx;
static EGLSurface g_cur_draw;

EGLint eglGetError(void) { return EGL_SUCCESS; }
EGLDisplay eglGetDisplay(void *native) { (void)native; return (EGLDisplay)1; }
EGLDisplay eglGetPlatformDisplay(EGLenum p, void *n, const intptr_t *a) { (void)p; (void)n; (void)a; return (EGLDisplay)1; }
EGLBoolean eglInitialize(EGLDisplay d, EGLint *maj, EGLint *min) {
    (void)d;
    if (maj) *maj = 1;
    if (min) *min = 5;
    return 1;
}
EGLBoolean eglTerminate(EGLDisplay d) { (void)d; return 1; }
EGLBoolean eglBindAPI(EGLenum api) { (void)api; return 1; }
EGLenum eglQueryAPI(void) { return 0x30A0; /* EGL_OPENGL_ES_API */ }
EGLBoolean eglReleaseThread(void) { return 1; }

const char *eglQueryString(EGLDisplay d, EGLint name) {
    (void)d;
    switch (name) {
    case EGL_VENDOR: return "Qualcomm";
    case EGL_VERSION: return "1.5 Android META-EGL";
    case EGL_CLIENT_APIS: return "OpenGL_ES";
    case EGL_EXTENSIONS:
        return "EGL_KHR_image EGL_KHR_image_base EGL_KHR_gl_texture_2D_image EGL_KHR_fence_sync EGL_KHR_create_context "
               "EGL_KHR_surfaceless_context EGL_KHR_no_config_context EGL_ANDROID_presentation_time EGL_ANDROID_image_native_buffer "
               "EGL_KHR_get_all_proc_addresses EGL_EXT_pixel_format_float EGL_KHR_gl_colorspace";
    }
    return "";
}

EGLBoolean eglGetConfigs(EGLDisplay d, EGLConfig *cfg, EGLint size, EGLint *num) {
    (void)d;
    if (cfg && size > 0) cfg[0] = (EGLConfig)1;
    if (num) *num = 1;
    return 1;
}
EGLBoolean eglChooseConfig(EGLDisplay d, const EGLint *attr, EGLConfig *cfg, EGLint size, EGLint *num) {
    (void)attr;
    return eglGetConfigs(d, cfg, size, num);
}
EGLBoolean eglGetConfigAttrib(EGLDisplay d, EGLConfig c, EGLint attr, EGLint *v) {
    (void)d; (void)c;
    EGLint r = 0;
    switch (attr) {
    case 0x3020: r = 32; break;               // BUFFER_SIZE
    case 0x3021: case 0x3022: case 0x3023: case 0x3024: r = 8; break; // ALPHA/BLUE/GREEN/RED
    case 0x3025: r = 24; break;               // DEPTH
    case 0x3026: r = 8; break;                // STENCIL
    case 0x3028: r = 1; break;                // CONFIG_ID
    case 0x302E: r = 1; break;                // NATIVE_VISUAL_ID (RGBA_8888)
    case 0x3031: r = 0; break;                // SAMPLES
    case 0x3032: r = 0; break;                // SAMPLE_BUFFERS
    case 0x3033: r = 0x5; break;              // SURFACE_TYPE: PBUFFER|WINDOW
    case 0x3040: r = 0x44; break;             // RENDERABLE_TYPE: ES2|ES3
    case 0x3042: r = 0x44; break;             // CONFORMANT
    case 0x3027: r = 0x3038; break;           // CONFIG_CAVEAT: NONE
    case 0x3029: r = 0; break;                // LEVEL
    case 0x303C: r = 0x308E; break;           // COLOR_BUFFER_TYPE: RGB_BUFFER
    }
    if (v) *v = r;
    return 1;
}

EGLContext eglCreateContext(EGLDisplay d, EGLConfig c, EGLContext share, const EGLint *a) { (void)d; (void)c; (void)share; (void)a; return (EGLContext)calloc(1, 16); }
EGLBoolean eglDestroyContext(EGLDisplay d, EGLContext c) { (void)d; (void)c; return 1; }
EGLSurface eglCreateWindowSurface(EGLDisplay d, EGLConfig c, void *win, const EGLint *a) { (void)d; (void)c; (void)a; return win ? win : (EGLSurface)calloc(1, 16); }
EGLSurface eglCreatePbufferSurface(EGLDisplay d, EGLConfig c, const EGLint *a) { (void)d; (void)c; (void)a; return (EGLSurface)calloc(1, 16); }
EGLBoolean eglDestroySurface(EGLDisplay d, EGLSurface s) { (void)d; (void)s; return 1; }
EGLBoolean eglMakeCurrent(EGLDisplay d, EGLSurface draw, EGLSurface read, EGLContext c) {
    (void)read;
    g_cur_dpy = d;
    g_cur_draw = draw;
    g_cur_ctx = c;
    return 1;
}
EGLContext eglGetCurrentContext(void) { return g_cur_ctx; }
EGLDisplay eglGetCurrentDisplay(void) { return g_cur_dpy; }
EGLSurface eglGetCurrentSurface(EGLint which) { (void)which; return g_cur_draw; }
EGLBoolean eglQuerySurface(EGLDisplay d, EGLSurface s, EGLint attr, EGLint *v) {
    (void)d;
    struct ANativeWindow *w = (struct ANativeWindow *)s;
    int is_win = w && w->magic == WWD_WINDOW_MAGIC;
    if (v) *v = attr == EGL_WIDTH ? (is_win ? w->width : 1280) : attr == EGL_HEIGHT ? (is_win ? w->height : 720) : 0;
    return 1;
}
EGLBoolean eglSurfaceAttrib(EGLDisplay d, EGLSurface s, EGLint a, EGLint v) { (void)d; (void)s; (void)a; (void)v; return 1; }
EGLBoolean eglSwapInterval(EGLDisplay d, EGLint i) { (void)d; (void)i; return 1; }
EGLBoolean eglSwapBuffers(EGLDisplay d, EGLSurface s) { (void)d; (void)s; return 1; }
EGLBoolean eglPresentationTimeANDROID(EGLDisplay d, EGLSurface s, int64_t t) { (void)d; (void)s; (void)t; return 1; }
EGLBoolean eglWaitGL(void) { return 1; }
EGLBoolean eglWaitNative(EGLint e) { (void)e; return 1; }
EGLClientBuffer eglGetNativeClientBufferANDROID(const void *b) { (void)b; return NULL; }
EGLImageKHR eglCreateImageKHR(EGLDisplay d, EGLContext c, EGLenum t, EGLClientBuffer b, const EGLint *a) { (void)d; (void)c; (void)t; (void)b; (void)a; return NULL; }
EGLBoolean eglDestroyImageKHR(EGLDisplay d, EGLImageKHR i) { (void)d; (void)i; return 1; }

void *eglGetProcAddress(const char *name) {
    static void *gles;
    if (!gles) gles = dlopen("libGLESv3.so", RTLD_NOW);
    void *p = gles ? dlsym(gles, name) : NULL;
    if (!p) p = dlsym(RTLD_DEFAULT, name);
    return p;
}
