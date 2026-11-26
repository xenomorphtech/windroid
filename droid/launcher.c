// wwd-app: runs an installed Android NativeActivity app (Unreal Engine games)
// without the Android framework. This process plays the role of the Java
// side: it loads the app's native library, replays GameActivity.onCreate's
// native calls, drives the NativeActivity lifecycle and pumps host input.
//
// usage: wwd-app <package> [engine args...]
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "jvm.h"
#include "wwd_host.h"

typedef struct ANativeWindow ANativeWindow;
typedef struct AInputQueue AInputQueue;
typedef struct AAssetManager AAssetManager;
typedef struct { int32_t left, top, right, bottom; } ARect;

struct ANativeActivity;
typedef struct ANativeActivityCallbacks {
    void (*onStart)(struct ANativeActivity *);
    void (*onResume)(struct ANativeActivity *);
    void *(*onSaveInstanceState)(struct ANativeActivity *, size_t *);
    void (*onPause)(struct ANativeActivity *);
    void (*onStop)(struct ANativeActivity *);
    void (*onDestroy)(struct ANativeActivity *);
    void (*onWindowFocusChanged)(struct ANativeActivity *, int);
    void (*onNativeWindowCreated)(struct ANativeActivity *, ANativeWindow *);
    void (*onNativeWindowResized)(struct ANativeActivity *, ANativeWindow *);
    void (*onNativeWindowRedrawNeeded)(struct ANativeActivity *, ANativeWindow *);
    void (*onNativeWindowDestroyed)(struct ANativeActivity *, ANativeWindow *);
    void (*onInputQueueCreated)(struct ANativeActivity *, AInputQueue *);
    void (*onInputQueueDestroyed)(struct ANativeActivity *, AInputQueue *);
    void (*onContentRectChanged)(struct ANativeActivity *, const ARect *);
    void (*onConfigurationChanged)(struct ANativeActivity *);
    void (*onLowMemory)(struct ANativeActivity *);
} ANativeActivityCallbacks;

typedef struct ANativeActivity {
    ANativeActivityCallbacks *callbacks;
    JavaVM *vm;
    JNIEnv *env;
    jobject clazz;
    const char *internalDataPath;
    const char *externalDataPath;
    int32_t sdkVersion;
    void *instance;
    AAssetManager *assetManager;
    const char *obbPath;
} ANativeActivity;

// from our libandroid
ANativeWindow *wwd_native_window_create(uint32_t w, uint32_t h);
AInputQueue *wwd_input_queue_create(void);
void wwd_input_queue_push(AInputQueue *q, int type, int action, float x, float y, int keycode);
void wwd_assets_init(const char *pkg_dir);
AAssetManager *wwd_asset_manager(void);

static void *g_lib;
static jobject g_activity;

#define NATIVE(name) "Java_com_epicgames_unreal_GameActivity_" name

static void *sym(const char *name) {
    void *p = dlsym(g_lib, name);
    // UE 5.7 registers its natives with RegisterNatives in JNI_OnLoad
    const char *pre = "Java_com_epicgames_unreal_GameActivity_";
    if (!p && !strncmp(name, pre, strlen(pre))) p = jvm_find_native("com/epicgames/unreal/GameActivity", name + strlen(pre));
    if (!p) wwd_logf("wwd-app", "missing native %s", name);
    return p;
}

static jstring S(const char *s) { return jvm_new_string(s); }

static int vk_to_keycode(uint32_t vk) {
    if (vk >= 'A' && vk <= 'Z') return 29 + (int)(vk - 'A');
    if (vk >= '0' && vk <= '9') return 7 + (int)(vk - '0');
    switch (vk) {
    case 0x1b: return 4;   // escape -> BACK
    case 0x0d: return 66;  // enter
    case 0x20: return 62;  // space
    case 0x08: return 67;  // backspace -> DEL
    case 0x09: return 61;  // tab
    case 0x25: return 21;  // left
    case 0x26: return 19;  // up
    case 0x27: return 22;  // right
    case 0x28: return 20;  // down
    case 0x10: return 59;  // shift
    case 0x11: return 113; // ctrl
    case 0xc0: return 68;  // ` (UE console)
    default: return 0;
    }
}

int main(int argc, char **argv) {
    const char *pkg = argc > 1 ? argv[1] : "com.nexon.overgeared";
    snprintf(wwd_pkg, sizeof wwd_pkg, "%s", pkg);
    snprintf(wwd_pkg_dir, sizeof wwd_pkg_dir, "/data/app/%s", pkg);
    // UE reads its command line as one string (with the project name first)
    size_t o = 0;
    for (int i = 2; i < argc && o < sizeof wwd_cmdline - 2; i++) o += (size_t)snprintf(wwd_cmdline + o, sizeof wwd_cmdline - o, "%s%s", o ? " " : "", argv[i]);
    if (getenv("WWD_JNI_TRACE")) wwd_jni_trace = 1;

    jvm_init();
    wwd_assets_init(wwd_pkg_dir);
    JNIEnv *env = wwd_env;
    g_activity = jvm_new_instance("com/epicgames/unreal/GameActivity");

    char lib[700], internal[512], external[512], obbdir[512], obb[700], apk[600];
    snprintf(lib, sizeof lib, "%s/lib/arm64/libUnreal.so", wwd_pkg_dir);
    snprintf(internal, sizeof internal, "/data/user/0/%s/files", pkg);
    snprintf(external, sizeof external, "/storage/emulated/0/Android/data/%s/files", pkg);
    snprintf(obbdir, sizeof obbdir, "/storage/emulated/0/Android/obb/%s", pkg);
    snprintf(apk, sizeof apk, "%s/base.apk", wwd_pkg_dir);

    // find the main OBB placed by install_apk.py
    obb[0] = 0;
    FILE *mf = NULL;
    char vcode[32] = "1";
    char mpath[600];
    snprintf(mpath, sizeof mpath, "%s/manifest.txt", wwd_pkg_dir);
    if ((mf = fopen(mpath, "r"))) {
        char line[512];
        while (fgets(line, sizeof line, mf))
            if (!strncmp(line, "versionCode\t", 12)) {
                snprintf(vcode, sizeof vcode, "%s", line + 12);
                vcode[strcspn(vcode, "\r\n")] = 0;
            }
        fclose(mf);
    }
    snprintf(obb, sizeof obb, "%s/main.%s.%s.obb", obbdir, vcode, pkg);
    if (access(obb, R_OK) != 0) obb[0] = 0;

    wwd_logf("wwd-app", "loading %s", lib);
    uint64_t t0 = wwd_host_monotonic_ns();
    g_lib = dlopen(lib, RTLD_NOW | RTLD_GLOBAL);
    if (!g_lib) {
        wwd_logf("wwd-app", "dlopen failed: %s", dlerror());
        return 1;
    }
    wwd_logf("wwd-app", "libUnreal loaded in %.1f s", (wwd_host_monotonic_ns() - t0) / 1e9);

    jint (*onload)(JavaVM *, void *) = (jint (*)(JavaVM *, void *))dlsym(g_lib, "JNI_OnLoad");
    if (onload) wwd_logf("wwd-app", "JNI_OnLoad -> %#x", onload(wwd_vm, NULL));

    // --- GameActivity.onCreate, native side ---
    void (*startup_state)(JNIEnv *, jobject, jboolean) = sym(NATIVE("nativeSetAndroidStartupState"));
    if (startup_state) startup_state(env, g_activity, 0);
    void (*set_global)(JNIEnv *, jobject, jboolean, jboolean, jstring, jstring, jboolean, jstring) = sym(NATIVE("nativeSetGlobalActivity"));
    if (set_global) set_global(env, g_activity, 1, 1, S(internal), S(external), 0, S(apk));
    void (*set_ver)(JNIEnv *, jobject, jstring, jint, jstring, jstring, jstring, jstring, jstring) = sym(NATIVE("nativeSetAndroidVersionInformation"));
    if (set_ver) set_ver(env, g_activity, S("14"), 34, S("wwd"), S("wwd Windows"), S("UP1A.231005.007"), S("en"), S("wwd"));
    void (*set_obb_paths)(JNIEnv *, jobject, jstring, jstring, jstring, jstring) = sym(NATIVE("nativeSetObbFilePaths"));
    if (set_obb_paths) set_obb_paths(env, g_activity, S(obb), S(""), S(""), S(""));
    void (*set_obb_info)(JNIEnv *, jobject, jstring, jstring, jint, jint, jstring) = sym(NATIVE("nativeSetObbInfo"));
    if (set_obb_info) set_obb_info(env, g_activity, S("Temppal"), S(pkg), atoi(vcode), 0, S(""));
    void (*set_window)(JNIEnv *, jobject, jboolean, jint, jint) = sym(NATIVE("nativeSetWindowInfo"));
    if (set_window) set_window(env, g_activity, 0, 0, 0);
    void (*set_surface)(JNIEnv *, jobject, jint, jint) = sym(NATIVE("nativeSetSurfaceViewInfo"));
    if (set_surface) set_surface(env, g_activity, 1280, 720);
    void (*set_safezone)(JNIEnv *, jobject, jboolean, jfloat, jfloat, jfloat, jfloat) = sym(NATIVE("nativeSetSafezoneInfo"));
    if (set_safezone) set_safezone(env, g_activity, 0, 0, 0, 0, 0);
    void (*set_affinity)(JNIEnv *, jobject, jboolean, jint, jint) = sym(NATIVE("nativeSetAffinityInfo"));
    if (set_affinity) set_affinity(env, g_activity, 0, 0, 0);
    void (*set_rules)(JNIEnv *, jobject, jobjectArray) = sym(NATIVE("nativeSetConfigRulesVariables"));
    if (set_rules) {
        // key/value pairs the Java side normally derives from the device
        static const char *const rules[] = {
            "unreal.displaymetrics.dpi", "320.0,320.0",
            "unreal.displaymetrics.metrics", "1280,720",
            // what GameActivity.N0() derives from the device for config rules
            "SRC_GpuFamily", "NVIDIA",
            "SRC_GlVersion", "OpenGL ES 3.2",
            "SRC_VulkanAvailable", "true",
            "SRC_VulkanVersion", "1.3.0",
        };
        int n = (int)(sizeof rules / sizeof rules[0]);
        jobjectArray arr = jvm_new_array('l', n);
        for (int i = 0; i < n; i++) ((jobject *)jvm_array_data(arr))[i] = S(rules[i]);
        set_rules(env, g_activity, arr);
    }

    // --- NativeActivity ---
    static ANativeActivityCallbacks cbs;
    static ANativeActivity act;
    act.callbacks = &cbs;
    act.vm = wwd_vm;
    act.env = env;
    act.clazz = g_activity;
    act.internalDataPath = strdup(internal);
    act.externalDataPath = strdup(external);
    act.sdkVersion = 34;
    act.assetManager = wwd_asset_manager();
    act.obbPath = strdup(obbdir);
    void (*on_create)(ANativeActivity *, void *, size_t) = sym("ANativeActivity_onCreate");
    if (!on_create) return 1;
    wwd_logf("wwd-app", "ANativeActivity_onCreate");
    on_create(&act, NULL, 0);
    if (cbs.onStart) cbs.onStart(&act);
    if (cbs.onResume) cbs.onResume(&act);

    AInputQueue *queue = wwd_input_queue_create();
    if (cbs.onInputQueueCreated) cbs.onInputQueueCreated(&act, queue);
    ANativeWindow *win = wwd_native_window_create(1280, 720);
    if (cbs.onNativeWindowCreated) cbs.onNativeWindowCreated(&act, win);
    if (cbs.onNativeWindowResized) cbs.onNativeWindowResized(&act, win);
    if (cbs.onContentRectChanged) {
        ARect r = {0, 0, win->width, win->height};
        cbs.onContentRectChanged(&act, &r);
    }
    if (cbs.onWindowFocusChanged) cbs.onWindowFocusChanged(&act, 1);

    // GameActivity.L0(): initial-download start, intent data, then resume main init
    void (*dl_started)(JNIEnv *, jobject) = sym(NATIVE("nativeOnInitialDownloadStarted"));
    if (dl_started) dl_started(env, g_activity);
    void (*intent_data)(JNIEnv *, jobject, jstring) = sym(NATIVE("nativeHandleNewIntentData"));
    if (intent_data) intent_data(env, g_activity, jvm_new_string(""));
    void (*resume_init)(JNIEnv *, jobject) = sym(NATIVE("nativeResumeMainInit"));
    if (resume_init) {
        wwd_logf("wwd-app", "nativeResumeMainInit");
        resume_init(env, g_activity);
    }
    // DownloaderActivity returned RESULT_OK: OBB present and verified
    void (*dl_done)(JNIEnv *, jobject) = sym(NATIVE("nativeOnInitialDownloadCompleted"));
    if (dl_done) {
        wwd_logf("wwd-app", "nativeOnInitialDownloadCompleted");
        dl_done(env, g_activity);
    }

    // --- the "UI thread": pump host window events ---
    for (;;) {
        wwd_event ev;
        int got = 0;
        while (wwd_host_window_poll(win->host_window, &ev)) {
            got = 1;
            if (getenv("WWD_INPUT_TRACE") && ev.kind <= 3) wwd_logf("wwd-input", "window event %d at %.0f,%.0f", ev.kind, ev.x, ev.y);
            switch (ev.kind) {
            case 1: wwd_input_queue_push(queue, 2, 0, ev.x, ev.y, 0); break;
            case 2: wwd_input_queue_push(queue, 2, 1, ev.x, ev.y, 0); break;
            case 3: wwd_input_queue_push(queue, 2, 2, ev.x, ev.y, 0); break;
            case 4:
            case 5: {
                int kc = vk_to_keycode(ev.key);
                if (kc) wwd_input_queue_push(queue, 1, ev.kind == 4 ? 0 : 1, 0, 0, kc);
                break;
            }
            case 6:
                win->width = (int32_t)ev.w;
                win->height = (int32_t)ev.h;
                if (cbs.onNativeWindowResized) cbs.onNativeWindowResized(&act, win);
                if (cbs.onContentRectChanged) {
                    ARect r = {0, 0, (int32_t)ev.w, (int32_t)ev.h};
                    cbs.onContentRectChanged(&act, &r);
                }
                break;
            case 7:
                wwd_logf("wwd-app", "window closed");
                if (cbs.onPause) cbs.onPause(&act);
                if (cbs.onStop) cbs.onStop(&act);
                exit(0);
            case 8: if (cbs.onWindowFocusChanged) cbs.onWindowFocusChanged(&act, 1); break;
            case 9: if (cbs.onWindowFocusChanged) cbs.onWindowFocusChanged(&act, 0); break;
            }
        }
        static int beat;
        if (getenv("WWD_INPUT_TRACE") && ++beat % 2000 == 0) wwd_logf("wwd-input", "ui loop alive");
        java_pump(env);
        if (!got) usleep(5000);
    }
}
