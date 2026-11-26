// Java-side behaviour for the fake JVM: the subset of GameActivity, Context,
// ClassLoader and friends that Unreal's native code calls.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "jvm.h"

char wwd_cmdline[4096];
char wwd_pkg[256] = "com.example.app";
char wwd_pkg_dir[512];

#define GA "com/epicgames/unreal/GameActivity"
void java_register_nexon(void);

static jvalue Z(int v) { jvalue r; r.j = 0; r.z = (jboolean)(v != 0); return r; }
static jvalue I(int v) { jvalue r; r.j = 0; r.i = v; return r; }
static jvalue J(int64_t v) { jvalue r; r.j = v; return r; }
static jvalue F(float v) { jvalue r; r.j = 0; r.f = v; return r; }
static jvalue L(jobject v) { jvalue r; r.j = 0; r.l = v; return r; }
static jvalue S(const char *s) { return L(jvm_new_string(s)); }
static const char *arg_str(const jvalue *a, int i) { const char *s = jvm_string_utf8(a[i].l); return s ? s : ""; }

// ---------------------------------------------------------------------------
// AndroidManifest <meta-data> (written by scripts/install_apk.py)
// ---------------------------------------------------------------------------

typedef struct meta { char *key; char type; char *val; struct meta *next; } meta;
static meta *g_meta;

static void load_metadata(void) {
    char path[600];
    snprintf(path, sizeof path, "%s/metadata.txt", wwd_pkg_dir);
    FILE *f = fopen(path, "r");
    if (!f) return;
    char line[2048];
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = 0;
        char *t1 = strchr(line, '\t');
        if (!t1) continue;
        *t1++ = 0;
        char *t2 = strchr(t1, '\t');
        if (!t2) continue;
        *t2++ = 0;
        meta *m = calloc(1, sizeof *m);
        m->key = strdup(line);
        m->type = t1[0];
        m->val = strdup(t2);
        m->next = g_meta;
        g_meta = m;
    }
    fclose(f);
}

static meta *meta_find(const char *k) {
    for (meta *m = g_meta; m; m = m->next)
        if (!strcmp(m->key, k)) return m;
    return NULL;
}

// GameActivity answers some "unreal.*" metadata keys with live device values.
static const char *live_string(const char *k) {
    if (!strcmp(k, "unreal.displaymetrics.dpi")) return "320.0,320.0";
    if (!strcmp(k, "unreal.displaymetrics.metrics"))
        return "widthPixels;1280;heightPixels;720;density;2.0;densityDpi;320;scaledDensity;2.0;xdpi;320.0;ydpi;320.0";
    if (!strcmp(k, "unreal.display"))
        return "realWidth;1280;realHeight;720;width;1280;height;720;rotation;0;refreshRate;60.0;displayId;0";
    if (!strcmp(k, "unreal.http.proxy.proxyHost")) return "";
    return NULL;
}
static int live_int(const char *k, int *out) {
    if (!strcmp(k, "unreal.getUsedMemory")) { *out = 512; return 1; }
    if (!strcmp(k, "unreal.http.proxy.proxyPort")) { *out = 0; return 1; }
    // PackageManager system features: Vulkan 1.3, hardware level 1
    if (!strcmp(k, "android.hardware.vulkan.version")) { *out = (1 << 22) | (3 << 12); return 1; }
    if (!strcmp(k, "android.hardware.vulkan.level")) { *out = 1; return 1; }
    return 0;
}

static jvalue md_bool(JNIEnv *e, jobject s, const jvalue *a) { meta *m = meta_find(arg_str(a, 0)); return Z(m && !strcmp(m->val, "true")); }
static jvalue md_int(JNIEnv *e, jobject s, const jvalue *a) {
    int v;
    if (live_int(arg_str(a, 0), &v)) return I(v);
    meta *m = meta_find(arg_str(a, 0));
    return I(m ? atoi(m->val) : 0);
}
static jvalue md_long(JNIEnv *e, jobject s, const jvalue *a) {
    if (!strcmp(arg_str(a, 0), "unreal.display.PresentationDeadlineNanos")) return J(11000000);
    meta *m = meta_find(arg_str(a, 0));
    return J(m ? atoll(m->val) : 0);
}
static jvalue md_float(JNIEnv *e, jobject s, const jvalue *a) {
    if (!strcmp(arg_str(a, 0), "unreal.display.getRefreshRate")) return F(60.0f);
    meta *m = meta_find(arg_str(a, 0));
    return F(m ? strtof(m->val, NULL) : 0.f);
}
static jvalue md_string(JNIEnv *e, jobject s, const jvalue *a) {
    const char *live = live_string(arg_str(a, 0));
    if (live) return S(live);
    meta *m = meta_find(arg_str(a, 0));
    return m ? S(m->val) : L(NULL);
}
static jvalue md_has(JNIEnv *e, jobject s, const jvalue *a) { return Z(meta_find(arg_str(a, 0)) != NULL); }

// ---------------------------------------------------------------------------
// shared preferences (in memory for now)
// ---------------------------------------------------------------------------

typedef struct pref { char *key; char *val; struct pref *next; } pref;
static pref *g_prefs;
static pref *pref_find(const char *grp, const char *k, int create) {
    char key[512];
    snprintf(key, sizeof key, "%s/%s", grp, k);
    for (pref *p = g_prefs; p; p = p->next)
        if (!strcmp(p->key, key)) return p;
    if (!create) return NULL;
    pref *p = calloc(1, sizeof *p);
    p->key = strdup(key);
    p->next = g_prefs;
    g_prefs = p;
    return p;
}
static void pref_set(const char *g, const char *k, const char *v) { pref *p = pref_find(g, k, 1); free(p->val); p->val = strdup(v); }
static jvalue pr_get_bool(JNIEnv *e, jobject s, const jvalue *a) { pref *p = pref_find(arg_str(a, 0), arg_str(a, 1), 0); return Z(p ? !strcmp(p->val, "1") : a[2].z); }
static jvalue pr_get_int(JNIEnv *e, jobject s, const jvalue *a) { pref *p = pref_find(arg_str(a, 0), arg_str(a, 1), 0); return I(p ? atoi(p->val) : a[2].i); }
static jvalue pr_get_str(JNIEnv *e, jobject s, const jvalue *a) { pref *p = pref_find(arg_str(a, 0), arg_str(a, 1), 0); return p ? S(p->val) : L(a[2].l); }
static jvalue pr_set_bool(JNIEnv *e, jobject s, const jvalue *a) { pref_set(arg_str(a, 0), arg_str(a, 1), a[2].z ? "1" : "0"); return J(0); }
static jvalue pr_set_int(JNIEnv *e, jobject s, const jvalue *a) { char b[32]; snprintf(b, sizeof b, "%d", a[2].i); pref_set(arg_str(a, 0), arg_str(a, 1), b); return J(0); }
static jvalue pr_set_str(JNIEnv *e, jobject s, const jvalue *a) { pref_set(arg_str(a, 0), arg_str(a, 1), arg_str(a, 2)); return J(0); }
static jvalue pr_has(JNIEnv *e, jobject s, const jvalue *a) { return Z(pref_find(arg_str(a, 0), arg_str(a, 1), 0) != NULL); }

// ---------------------------------------------------------------------------
// GameActivity
// ---------------------------------------------------------------------------

static jvalue zero(JNIEnv *e, jobject s, const jvalue *a) { return J(0); }
static jvalue empty_str(JNIEnv *e, jobject s, const jvalue *a) { return S(""); }
static jvalue get_asset_manager(JNIEnv *e, jobject s, const jvalue *a) {
    static jobject am;
    if (!am) am = jvm_new_instance("android/content/res/AssetManager");
    return L(am);
}
static jvalue get_cache_dir(JNIEnv *e, jobject s, const jvalue *a) { char b[512]; snprintf(b, sizeof b, "/data/user/0/%s/cache", wwd_pkg); return S(b); }
static jvalue get_cmdline(JNIEnv *e, jobject s, const jvalue *a) { return S(wwd_cmdline); }
static jvalue get_font_dir(JNIEnv *e, jobject s, const jvalue *a) { return S("/system/fonts/"); }
static jvalue get_android_id(JNIEnv *e, jobject s, const jvalue *a) { return S("77dd0000c0ffee01"); }
static jvalue refresh_rate(JNIEnv *e, jobject s, const jvalue *a) { return I(60); }
static jvalue refresh_rates(JNIEnv *e, jobject s, const jvalue *a) {
    jobject arr = jvm_new_array('i', 1);
    ((jint *)jvm_array_data(arr))[0] = 60;
    return L(arr);
}
static jvalue thermal_headroom(JNIEnv *e, jobject s, const jvalue *a) { return F(0.3f); }
#include <dlfcn.h>
// Frame-pointer backtrace of the guest (AAPCS64 frame records), symbolized as lib+offset.
void wwd_backtrace(const char *why) {
    wwd_logf("wwd-bt", "backtrace (%s):", why);
    uintptr_t *fp = (uintptr_t *)__builtin_frame_address(0);
    for (int i = 0; fp && i < 40; i++) {
        uintptr_t lr = fp[1] & 0x00ffffffffffffffull;
        if (!lr) break;
        Dl_info di;
        if (dladdr((void *)lr, &di) && di.dli_fname) {
            const char *b = strrchr(di.dli_fname, '/');
            wwd_logf("wwd-bt", "  #%02d %s+0x%lx", i, b ? b + 1 : di.dli_fname, (unsigned long)(lr - (uintptr_t)di.dli_fbase));
        } else {
            wwd_logf("wwd-bt", "  #%02d 0x%lx", i, (unsigned long)lr);
        }
        uintptr_t *next = (uintptr_t *)(fp[0] & 0x00ffffffffffffffull);
        if (next <= fp) break;
        fp = next;
    }
}
static jvalue force_quit(JNIEnv *e, jobject s, const jvalue *a) {
    wwd_logf("wwd", "game requested ForceQuit");
    wwd_backtrace("ForceQuit");
    _exit(0);
}
static jvalue get_class_loader(JNIEnv *e, jobject s, const jvalue *a) {
    static jobject cl;
    if (!cl) cl = jvm_new_instance("java/lang/ClassLoader");
    return L(cl);
}
static jvalue load_class(JNIEnv *e, jobject s, const jvalue *a) { return L(jvm_find_class(arg_str(a, 0))); }
static jvalue get_package_name(JNIEnv *e, jobject s, const jvalue *a) { return S(wwd_pkg); }
static jvalue self_obj(JNIEnv *e, jobject s, const jvalue *a) { return L(s); }

static jvalue file_for(const char *path) {
    jobject f = jvm_new_instance("java/io/File");
    jvm_field_set(f, jvm_field_id("java/io/File", "path", "Ljava/lang/String;"), L(jvm_new_string(path)));
    return L(f);
}
static jvalue file_path(JNIEnv *env, jobject s, const jvalue *a) {
    return jvm_field_get(s, jvm_field_id("java/io/File", "path", "Ljava/lang/String;"));
}
static jvalue files_dir(JNIEnv *e, jobject s, const jvalue *a) { char b[512]; snprintf(b, sizeof b, "/data/user/0/%s/files", wwd_pkg); return file_for(b); }
static jvalue cache_dir_file(JNIEnv *e, jobject s, const jvalue *a) { char b[512]; snprintf(b, sizeof b, "/data/user/0/%s/cache", wwd_pkg); return file_for(b); }
static jvalue ext_files_dir(JNIEnv *e, jobject s, const jvalue *a) { char b[512]; snprintf(b, sizeof b, "/storage/emulated/0/Android/data/%s/files", wwd_pkg); return file_for(b); }
static jvalue obb_dir(JNIEnv *e, jobject s, const jvalue *a) { char b[512]; snprintf(b, sizeof b, "/storage/emulated/0/Android/obb/%s", wwd_pkg); return file_for(b); }
static jvalue ext_storage(JNIEnv *e, jobject s, const jvalue *a) { return file_for("/storage/emulated/0"); }
static jvalue class_get_name(JNIEnv *e, jobject s, const jvalue *a) {
    jclass_impl *c = ((jobj *)s)->self_class;
    char b[512];
    snprintf(b, sizeof b, "%s", c ? c->name : "java.lang.Object");
    for (char *p = b; *p; p++)
        if (*p == '/') *p = '.';
    return S(b);
}
static jvalue object_get_class(JNIEnv *e, jobject s, const jvalue *a) {
    jobj *o = (jobj *)s;
    return L(o && o->cls ? o->cls->obj : jvm_find_class("java/lang/Object"));
}
static jvalue string_equals(JNIEnv *e, jobject s, const jvalue *a) {
    const char *x = jvm_string_utf8(s), *y = jvm_string_utf8(a[0].l);
    return Z(x && y && !strcmp(x, y));
}

static jvalue get_wm(JNIEnv *e, jobject s, const jvalue *a) { static jobject o; if (!o) o = jvm_new_instance("android/view/WindowManager"); return L(o); }
static jvalue get_display(JNIEnv *e, jobject s, const jvalue *a) { static jobject o; if (!o) o = jvm_new_instance("android/view/Display"); return L(o); }
static jvalue display_refresh(JNIEnv *e, jobject s, const jvalue *a) { return F(60.0f); }
static jvalue display_deadline(JNIEnv *e, jobject s, const jvalue *a) { return J(11000000); }

// PackageManager / PackageInfo / ApplicationInfo
#define PKG "com.nexon.overgeared"
static jvalue get_pm(JNIEnv *e, jobject s, const jvalue *a) { static jobject o; if (!o) o = jvm_new_instance("android/content/pm/PackageManager"); return L(o); }
static jvalue get_app_info(JNIEnv *e, jobject s, const jvalue *a) {
    static jobject o;
    if (!o) {
        const char *c = "android/content/pm/ApplicationInfo";
        o = jvm_new_instance(c);
        jvm_field_set(o, jvm_field_id(c, "packageName", "Ljava/lang/String;"), L(jvm_new_string(PKG)));
        jvm_field_set(o, jvm_field_id(c, "sourceDir", "Ljava/lang/String;"), L(jvm_new_string("/data/app/" PKG "/base.apk")));
        jvm_field_set(o, jvm_field_id(c, "publicSourceDir", "Ljava/lang/String;"), L(jvm_new_string("/data/app/" PKG "/base.apk")));
        jvm_field_set(o, jvm_field_id(c, "nativeLibraryDir", "Ljava/lang/String;"), L(jvm_new_string("/data/app/" PKG "/lib/arm64")));
        jvm_field_set(o, jvm_field_id(c, "dataDir", "Ljava/lang/String;"), L(jvm_new_string("/data/user/0/" PKG)));
        jvm_field_set(o, jvm_field_id(c, "targetSdkVersion", "I"), I(34));
        jvm_field_set(o, jvm_field_id(c, "flags", "I"), I(0));
    }
    return L(o);
}
static jvalue get_pkg_info(JNIEnv *e, jobject s, const jvalue *a) {
    static jobject o;
    if (!o) {
        const char *c = "android/content/pm/PackageInfo";
        o = jvm_new_instance(c);
        jvm_field_set(o, jvm_field_id(c, "packageName", "Ljava/lang/String;"), L(jvm_new_string(PKG)));
        jvm_field_set(o, jvm_field_id(c, "versionName", "Ljava/lang/String;"), L(jvm_new_string("1.0.5")));
        jvm_field_set(o, jvm_field_id(c, "versionCode", "I"), I(183));
        jvm_field_set(o, jvm_field_id(c, "applicationInfo", "Landroid/content/pm/ApplicationInfo;"), get_app_info(e, s, a));
    }
    return L(o);
}

// com.graygames.graychunk.GrayNetworkManager: report a connected Wi-Fi network
static jvalue gray_network_type(JNIEnv *e, jobject s, const jvalue *a) { return S("WiFi"); }
static jvalue gray_start_listening(JNIEnv *e, jobject s, const jvalue *a) {
    void (*cb)(JNIEnv *, jclass, jstring) = (void (*)(JNIEnv *, jclass, jstring))dlsym(RTLD_DEFAULT, "Java_com_graygames_graychunk_GrayNetworkManager_nativeOnNetworkChanged");
    wwd_logf("wwd", "GrayNetworkManager.startListening -> nativeOnNetworkChanged(WiFi) %s", cb ? "" : "(native missing)");
    if (cb) cb(e, jvm_find_class("com/graygames/graychunk/GrayNetworkManager"), jvm_new_string("WiFi"));
    return J(0);
}
static jvalue net_type_wifi(JNIEnv *e, jobject s, const jvalue *a) { return I(4); /* ENetworkConnectionType::WiFi */ }

static void reg(const char *name, const char *sig, java_fn fn) { jvm_register(GA, name, sig, fn, 0); }

void java_register_all(void) {
    if (wwd_pkg_dir[0]) load_metadata();

    // --- meta-data and preferences ---
    reg("HasExtraIgnoreView", "()Z", zero); // no extra overlay view to exclude from input
    reg("AndroidThunkJava_GetMetaDataBoolean", "(Ljava/lang/String;)Z", md_bool);
    reg("AndroidThunkJava_GetMetaDataInt", "(Ljava/lang/String;)I", md_int);
    reg("AndroidThunkJava_GetMetaDataLong", "(Ljava/lang/String;)J", md_long);
    reg("AndroidThunkJava_GetMetaDataFloat", "(Ljava/lang/String;)F", md_float);
    reg("AndroidThunkJava_GetMetaDataString", "(Ljava/lang/String;)Ljava/lang/String;", md_string);
    reg("AndroidThunkJava_HasMetaDataKey", "(Ljava/lang/String;)Z", md_has);
    reg("AndroidThunkJava_GetSharedPreferenceBoolean", "(Ljava/lang/String;Ljava/lang/String;Z)Z", pr_get_bool);
    reg("AndroidThunkJava_GetSharedPreferenceInt", "(Ljava/lang/String;Ljava/lang/String;I)I", pr_get_int);
    reg("AndroidThunkJava_GetSharedPreferenceString", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;", pr_get_str);
    reg("AndroidThunkJava_SetSharedPreferenceBoolean", "(Ljava/lang/String;Ljava/lang/String;Z)V", pr_set_bool);
    reg("AndroidThunkJava_SetSharedPreferenceInt", "(Ljava/lang/String;Ljava/lang/String;I)V", pr_set_int);
    reg("AndroidThunkJava_SetSharedPreferenceString", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V", pr_set_str);
    reg("AndroidThunkJava_HasSharedPreference", "(Ljava/lang/String;Ljava/lang/String;)Z", pr_has);

    // --- device / environment ---
    reg("AndroidThunkJava_GetAssetManager", "()Landroid/content/res/AssetManager;", get_asset_manager);
    reg("AndroidThunkJava_GetCacheDir", "()Ljava/lang/String;", get_cache_dir);
    reg("AndroidThunkJava_GetCommandLine", "()Ljava/lang/String;", get_cmdline);
    jvm_register(GA, "AndroidThunkJava_GetFontDirectory", "()Ljava/lang/String;", get_font_dir, 1);
    reg("AndroidThunkJava_GetAndroidId", "()Ljava/lang/String;", get_android_id);
    reg("AndroidThunkJava_GetNativeDisplayRefreshRate", "()I", refresh_rate);
    reg("AndroidThunkJava_GetSupportedNativeDisplayRefreshRates", "()[I", refresh_rates);
    reg("AndroidThunkJava_getThermalHeadroom", "(I)F", thermal_headroom);
    reg("AndroidThunkJava_ForceQuit", "()V", force_quit);
    const char *empty_strings[] = {"AndroidThunkJava_GetLoginId", "AndroidThunkJava_GetFunnelId", "AndroidThunkJava_GetCellularType",
                                   "AndroidThunkJava_GetIntentDataAsString", "AndroidThunkJava_GetSafezoneDiagnostics",
                                   "AndroidThunkJava_ClipboardPaste", NULL};
    for (int i = 0; empty_strings[i]; i++) reg(empty_strings[i], "()Ljava/lang/String;", empty_str);

    // methods whose "nothing happened / false / 0" answer is correct for us
    static const char *const quiet[][2] = {
        {"AndroidThunkJava_AcquireWifiManagerMulticastLock", "()Z"}, {"AndroidThunkJava_AddNetworkListener", "()V"},
        {"AndroidThunkJava_RemoveNetworkListener", "()V"}, {"AndroidThunkJava_ReleaseWifiManagerMulticastLock", "()V"},
        {"AndroidThunkJava_DismissSplashScreen", "()V"}, {"AndroidThunkJava_KeepScreenOn", "(Z)V"},
        {"AndroidThunkJava_SetSustainedPerformanceMode", "(Z)V"}, {"AndroidThunkJava_EnableMotion", "(Z)V"},
        {"AndroidThunkJava_InitHMDs", "()V"}, {"AndroidThunkJava_IsGamepadAttached", "()Z"},
        {"AndroidThunkJava_IsMusicActive", "()Z"}, {"AndroidThunkJava_IsScreensaverEnabled", "()Z"},
        {"AndroidThunkJava_IsScreenCaptureDisabled", "()Z"}, {"AndroidThunkJava_DisableScreenCapture", "(Z)V"},
        {"AndroidThunkJava_GooglePAD_Available", "()Z"}, {"AndroidThunkJava_GetDeviceOrientation", "()I"},
        {"AndroidThunkJava_GetDeviceRotation", "()I"}, {"AndroidThunkJava_GetNetworkConnectionType", "()I"},
        {"AndroidThunkJava_HasIntentExtrasKey", "(Ljava/lang/String;)Z"}, {"AndroidThunkJava_GetIntentExtrasBoolean", "(Ljava/lang/String;)Z"},
        {"AndroidThunkJava_GetIntentExtrasInt", "(Ljava/lang/String;)I"}, {"AndroidThunkJava_IapIsAllowedToMakePurchases", "()Z"},
        {"AndroidThunkJava_IapSetupService", "(Ljava/lang/String;)V"}, {"AndroidThunkJava_IapQueryExistingPurchases", "()Z"},
        {"AndroidThunkJava_IsAllowedRemoteNotifications", "()Z"}, {"AndroidThunkJava_RegisterForRemoteNotifications", "()V"},
        {"AndroidThunkJava_UnregisterForRemoteNotifications", "()V"}, {"AndroidThunkJava_LocalNotificationClearAll", "()V"},
        {"AndroidThunkJava_LocalNotificationGetLaunchNotification", "()Lcom/epicgames/unreal/GameActivity$LaunchNotification;"},
        {"AndroidThunkJava_GetInputDeviceInfo", "(I)Lcom/epicgames/unreal/GameActivity$InputDeviceInfo;"},
        {"AndroidThunkJava_PushSensorEvents", "()V"}, {"AndroidThunkJava_SetOrientation", "(I)V"},
        {"AndroidThunkJava_SetDesiredViewSize", "(II)V"}, {"AndroidThunkJava_UseSurfaceViewWorkaround", "()V"},
        {"AndroidThunkJava_ShowProgressDialog", "(ZLjava/lang/String;ZI)V"}, {"AndroidThunkJava_UpdateProgressDialog", "(I)V"},
        {"AndroidThunkJava_Vibrate", "(II)V"}, {"AndroidThunkJava_IsPackageInstalled", "(Ljava/lang/String;)Z"},
        {"AndroidThunkJava_SetNativeDisplayRefreshRate", "(I)Z"}, {"AndroidThunkJava_HideVirtualKeyboardInput", "()V"},
        {NULL, NULL}};
    for (int i = 0; quiet[i][0]; i++) reg(quiet[i][0], quiet[i][1], zero);
    reg("AndroidThunkJava_GetIntentExtrasString", "(Ljava/lang/String;)Ljava/lang/String;", empty_str);

    // --- Context / Activity basics ---
    const char *ctx[] = {GA, "android/app/Activity", "android/content/Context", "android/app/NativeActivity", NULL};
    for (int i = 0; ctx[i]; i++) {
        jvm_register(ctx[i], "getClassLoader", "()Ljava/lang/ClassLoader;", get_class_loader, 0);
        jvm_register(ctx[i], "getPackageName", "()Ljava/lang/String;", get_package_name, 0);
        jvm_register(ctx[i], "getApplicationContext", "()Landroid/content/Context;", self_obj, 0);
        jvm_register(ctx[i], "getAssets", "()Landroid/content/res/AssetManager;", get_asset_manager, 0);
        jvm_register(ctx[i], "getFilesDir", "()Ljava/io/File;", files_dir, 0);
        jvm_register(ctx[i], "getCacheDir", "()Ljava/io/File;", cache_dir_file, 0);
        jvm_register(ctx[i], "getExternalFilesDir", "(Ljava/lang/String;)Ljava/io/File;", ext_files_dir, 0);
        jvm_register(ctx[i], "getObbDir", "()Ljava/io/File;", obb_dir, 0);
    }
    jvm_register("java/lang/ClassLoader", "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", load_class, 0);
    jvm_register("java/lang/ClassLoader", "findClass", "(Ljava/lang/String;)Ljava/lang/Class;", load_class, 0);
    jvm_register("java/lang/Class", "getName", "()Ljava/lang/String;", class_get_name, 0);
    jvm_register("java/lang/Object", "getClass", "()Ljava/lang/Class;", object_get_class, 0);
    jvm_register("java/lang/String", "equals", "(Ljava/lang/Object;)Z", string_equals, 0);
    jvm_register("android/os/Environment", "getExternalStorageDirectory", "()Ljava/io/File;", ext_storage, 1);
    jvm_register("java/io/File", "getAbsolutePath", "()Ljava/lang/String;", file_path, 0);
    jvm_register("java/io/File", "getPath", "()Ljava/lang/String;", file_path, 0);

    reg("getPackageManager", "()Landroid/content/pm/PackageManager;", get_pm);
    reg("getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", get_app_info);
    jvm_register("android/content/Context", "getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", get_app_info, 0);
    jvm_register("android/content/pm/PackageManager", "getPackageInfo", "(Ljava/lang/String;I)Landroid/content/pm/PackageInfo;", get_pkg_info, 0);
    jvm_register("java/lang/Object", "getPackageInfo", "(Ljava/lang/String;I)Landroid/content/pm/PackageInfo;", get_pkg_info, 0);
    jvm_register("com/graygames/graychunk/GrayNetworkManager", "getNetworkType", "(Landroid/app/Activity;)Ljava/lang/String;", gray_network_type, 1);
    jvm_register("com/graygames/graychunk/GrayNetworkManager", "startListening", "(Landroid/app/Activity;)V", gray_start_listening, 1);
    jvm_register("com/graygames/graychunk/GrayNetworkManager", "stopListening", "(Landroid/app/Activity;)V", zero, 1);
    reg("AndroidThunkJava_GetNetworkConnectionType", "()I", net_type_wifi);

    // WindowManager -> Display
    const char *wm_owners[] = {GA, "android/app/NativeActivity", "android/app/Activity", NULL};
    for (int i = 0; wm_owners[i]; i++) jvm_register(wm_owners[i], "getWindowManager", "()Landroid/view/WindowManager;", get_wm, 0);
    jvm_register("android/view/WindowManager", "getDefaultDisplay", "()Landroid/view/Display;", get_display, 0);
    jvm_register("android/view/Display", "getRefreshRate", "()F", display_refresh, 0);
    jvm_register("android/view/Display", "getAppVsyncOffsetNanos", "()J", zero, 0);
    jvm_register("android/view/Display", "getPresentationDeadlineNanos", "()J", display_deadline, 0);
    jvm_register("android/view/Display", "getRotation", "()I", zero, 0);

    // android.os.Build.VERSION.SDK_INT etc.
    jvm_static_field("android/os/Build$VERSION", "SDK_INT", "I", I(34));
    jvm_static_field("android/os/Build$VERSION", "RELEASE", "Ljava/lang/String;", S("14"));
    jvm_static_field("android/os/Build", "MANUFACTURER", "Ljava/lang/String;", S("wwd"));
    jvm_static_field("android/os/Build", "MODEL", "Ljava/lang/String;", S("wwd Windows"));
    java_register_nexon();
}

// ---------------------------------------------------------------------------
// Nexon SDK stand-in
//
// The game drives the Nexon Java SDK (kr.co.nexon.*) through static helper
// methods that take "JNISupport" listener objects wrapping a native peer.
// We don't run the SDK; for any SDK call we don't implement, every listener
// argument gets a success result posted to the UI thread, the way the SDK
// would answer.
// ---------------------------------------------------------------------------
#include <dlfcn.h>
#include <pthread.h>

struct posted {
    char cls[160];
    jlong ptr;
    jint tag;
    char json[1024];
    struct posted *next;
};
static pthread_mutex_t g_post_mu = PTHREAD_MUTEX_INITIALIZER;
static struct posted *g_post_head, **g_post_tail = &g_post_head;

static void *java_native(const char *cls, const char *name) {
    void *f = jvm_find_native(cls, name);
    if (f) return f;
    char sym[400] = "Java_";
    size_t o = 5;
    for (const char *p = cls; *p && o < sizeof sym - 2; p++) sym[o++] = *p == '/' ? '_' : *p;
    snprintf(sym + o, sizeof sym - o, "_%s", name);
    return dlsym(RTLD_DEFAULT, sym);
}

// Synthetic local identity (matches the local server's bootstrap LocalIdentity).
#define LOCAL_NPSN "1001"
#define LOCAL_TOKEN "LOCAL_TEST_TOKEN"
#define LOCAL_EMAIL "local@localhost"

static const char *nexon_result_payload(const char *method, int tag) {
    if (strstr(method, "LocatedCountry") || strstr(method, "Country")) return "{\"country\":\"KR\",\"countryCode\":\"KR\"}";
    // Gson form of NXToyUserInfoResult.ResultSet as NXPListenerJNISupport
    // delivers it: numeric npsn/NexonSN rewritten to strings
    // (replaceNumberToString), every primitive field present.
    if (!strcmp(method, "getUserInfo"))
        return "{\"email\":\"" LOCAL_EMAIL "\",\"guid\":\"" LOCAL_NPSN "\","
               "\"nkUserInfo\":{\"NEXON_ATL_0\":0,\"NEXON_ATL_1\":1,\"NEXON_ATL_2\":2,\"Age\":0,\"HiddenEmail\":\"\",\"HiddenLoginId\":\"\","
               "\"MainAuthLevel\":0,\"Membership\":false,\"NexonID\":\"\",\"NexonProfileDetailImageUrl\":\"\","
               "\"NexonProfileThumbnailImageUrl_130\":\"\",\"NexonSN\":\"" LOCAL_NPSN "\",\"NexonSNString\":\"" LOCAL_NPSN "\","
               "\"NexonTPAMemType\":0,\"NickName\":\"Local\",\"Sex\":0,\"SubAuthLevel\":0,\"WriteStatus\":0},"
               "\"npToken\":\"" LOCAL_TOKEN "\",\"npaCode\":\"KRPC\",\"npsn\":\"" LOCAL_NPSN "\",\"npsnString\":\"" LOCAL_NPSN "\","
               "\"npsnUserInfo\":{\"agcId\":\"\",\"age_range_max\":0,\"age_range_min\":0,\"birthDay\":\"\",\"email\":\"" LOCAL_EMAIL "\","
               "\"fbBizToken\":\"\",\"firstName\":\"\",\"gcid\":\"\",\"gender\":0,\"gpgId\":\"\",\"guid\":\"" LOCAL_NPSN "\","
               "\"lastName\":\"\",\"memID\":\"" LOCAL_NPSN "\",\"memType\":1,\"middleName\":\"\",\"name\":\"Local\","
               "\"npsn\":\"" LOCAL_NPSN "\",\"pictureUrl\":\"\",\"signUpCountry\":\"KR\",\"subID\":\"\",\"thirdPartyToken\":\"\"},"
               "\"pushAgree\":0}";
    if (tag == 56) return "{\"status\":\"1\"}"; // device notification status
    return "{}";
}

static jvalue nx_login_type(JNIEnv *e, jobject s, const jvalue *a) { return I(1); }
static jvalue nx_country_number(JNIEnv *e, jobject s, const jvalue *a) {
    const char *v = getenv("WWD_NX_COUNTRY");
    return I(v ? atoi(v) : 82);
}
static jvalue nx_country_letter(JNIEnv *e, jobject s, const jvalue *a) { return S("KR"); }
static jvalue nx_locale_letter(JNIEnv *e, jobject s, const jvalue *a) { return S(getenv("WWD_NX_LOCALE") ? getenv("WWD_NX_LOCALE") : "ko_KR"); }
// NexonSnType = table[getAuthenticationEnvironment() - 1] (libUnreal 0x82a6c5c)
static jvalue nx_auth_env(JNIEnv *e, jobject s, const jvalue *a) {
    const char *v = getenv("WWD_NX_AUTH_ENV");
    return I(v ? atoi(v) : 3); // 3 = KRPC (1 TOYNPSN, 2 TPA, 4 JPPC, 5 ARENA)
}
static jvalue nx_service_id(JNIEnv *e, jobject s, const jvalue *a) { return S("2585"); }
static jvalue nx_uuid(JNIEnv *e, jobject s, const jvalue *a) { return S("77dd0000-c0ff-ee01-0000-000000001001"); }
static jvalue nx_store_type(JNIEnv *e, jobject s, const jvalue *a) { return S("GooglePlay"); }
static jvalue nx_get_npsn(JNIEnv *e, jobject s, const jvalue *a) { return S(LOCAL_NPSN); }
static jvalue nx_file_root(JNIEnv *e, jobject s, const jvalue *a) {
    return S("/storage/emulated/0/Android/data/com.nexon.overgeared/files/PUB/Resource");
}
static jvalue nx_file_path(JNIEnv *e, jobject s, const jvalue *a) {
    char p[1024];
    const char *n = jvm_string_utf8(a[1].l);
    snprintf(p, sizeof p, "/storage/emulated/0/Android/data/com.nexon.overgeared/files/PUB/Resource/%s", n ? n : "");
    return S(p);
}

static jvalue create_gcm(JNIEnv *e, jobject s, const jvalue *a);

void java_register_nexon(void) {
    const char *B = "kr/co/nexon/npaccount/jnisupport/NPAccountJNISupport";
    jvm_register(B, "getLoginType", "(Lkr/co/nexon/npaccount/NPAccount;)I", nx_login_type, 1);
    jvm_register(B, "getCountry", "(Lkr/co/nexon/npaccount/NPAccount;)I", nx_country_number, 1);
    jvm_register(B, "getCountryLetterCode", "(Lkr/co/nexon/npaccount/NPAccount;)Ljava/lang/String;", nx_country_letter, 1);
    jvm_register("com/epicgames/unreal/GameActivity", "createGameControllerManager", "()Z", create_gcm, 0);
    jvm_register(B, "getLocaleLetterCode", "(Lkr/co/nexon/npaccount/NPAccount;)Ljava/lang/String;", nx_locale_letter, 1);
    jvm_register(B, "getAuthenticationEnvironment", "(Lkr/co/nexon/npaccount/NPAccount;)I", nx_auth_env, 1);
    jvm_register(B, "getServiceId", "(Lkr/co/nexon/npaccount/NPAccount;)Ljava/lang/String;", nx_service_id, 1);
    jvm_register(B, "getNpsn", "(Lkr/co/nexon/npaccount/NPAccount;)Ljava/lang/String;", nx_get_npsn, 1);
    jvm_register("com/nexon/pub/bar/NXPatcher", "getFileRoot", "(Landroid/content/Context;)Ljava/lang/String;", nx_file_root, 1);
    jvm_register("com/nexon/pub/bar/NXPatcher", "getFilePath", "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;", nx_file_path, 1);
}

// ---- Nexon patcher (com.nexon.patcher.JavaNXPatcher) stand-in: everything is up to date ----
#define NXP "com/nexon/patcher/JavaNXPatcher"
static char g_patch_step[64][48];
static int g_patch_n;
static pthread_mutex_t g_patch_mu = PTHREAD_MUTEX_INITIALIZER;

static void patcher_answer(JNIEnv *e, const char *step) {
    jclass k = jvm_find_class(NXP);
    jstring empty = jvm_new_string("");
    jarray no_str = jvm_new_array('L', 0), no_int = jvm_new_array('I', 0);
    void *f = NULL;
    wwd_logf("wwd-nexon", "patcher %s -> up to date", step);
    if (!strcmp(step, "init")) {
        if ((f = java_native(NXP, "nativePatcherInitResult"))) ((void (*)(JNIEnv *, jclass, jboolean, jint, jstring))f)(e, k, 1, 0, empty);
    } else if (!strcmp(step, "checkBuild")) {
        if ((f = java_native(NXP, "nativeCheckBuildResult"))) ((void (*)(JNIEnv *, jclass, jint, jint, jstring))f)(e, k, 0, 0, empty);
    } else if (!strcmp(step, "checkPatchVersion")) {
        if ((f = java_native(NXP, "nativeCheckPatchVersionResult"))) ((void (*)(JNIEnv *, jclass, jint, jint, jstring))f)(e, k, 0, 0, empty);
    } else if (!strcmp(step, "checkVersion")) {
        if ((f = java_native(NXP, "nativeCheckVersionOnResult")))
            ((void (*)(JNIEnv *, jclass, jlong, jlong, jint, jint, jlong, jlong, jint, jint, jint, jint))f)(e, k, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    } else if (!strcmp(step, "checkResource")) {
        if ((f = java_native(NXP, "nativeCheckResourceResult")))
            ((void (*)(JNIEnv *, jclass, jlong, jlong, jint, jint, jlong, jlong, jint, jint, jint, jobject, jobject, jint, jint, jstring))f)(
                e, k, 0, 0, 0, 0, 0, 0, 0, 0, 0, no_str, no_int, 0, 0, empty);
    } else if (!strcmp(step, "validateResource") || !strcmp(step, "verifyResource")) {
        if ((f = java_native(NXP, "nativeValidateResourceResult"))) ((void (*)(JNIEnv *, jclass, jint, jstring))f)(e, k, 0, empty);
    } else if (!strcmp(step, "startDownload") || !strcmp(step, "resumeDownload")) {
        if ((f = java_native(NXP, "nativeNewOnComplete"))) ((void (*)(JNIEnv *, jclass, jobject, jobject, jint, jint, jstring))f)(e, k, no_str, no_str, 0, 0, empty);
        if ((f = java_native(NXP, "nativeDownloadOnComplete"))) ((void (*)(JNIEnv *, jclass, jobject, jobject))f)(e, k, no_str, no_str);
    } else {
        f = (void *)1;
    }
    if (!f) wwd_logf("wwd-nexon", "patcher %s: native callback missing", step);
}

static const char *const k_patch_steps[] = {"init", "checkBuild", "checkPatchVersion", "checkVersion", "checkResource",
                                            "validateResource", "verifyResource", "startDownload", "resumeDownload", NULL};

void java_unimplemented_hook(JNIEnv *env, const char *cls, const char *name, const char *sig, const jvalue *args) {
    // JavaNXPatcher instance methods (the game's class lookup shows up as "None")
    if (!strcmp(cls, NXP) || !strcmp(cls, "None")) {
        for (int i = 0; k_patch_steps[i]; i++)
            if (!strcmp(name, k_patch_steps[i]) && !strncmp(sig, "(Landroid/content/Context;", 26)) {
                pthread_mutex_lock(&g_patch_mu);
                if (g_patch_n < 64) snprintf(g_patch_step[g_patch_n++], 48, "%s", name);
                pthread_mutex_unlock(&g_patch_mu);
                return;
            }
    }
    if (strncmp(cls, "kr/co/nexon/", 12) && strncmp(cls, "com/nexon/", 10)) return;
    const char *p = strchr(sig, '(') + 1;
    for (int i = 0; *p && *p != ')'; i++) {
        if (*p == 'L') {
            jobject o = args[i].l;
            const char *cn = o ? jvm_object_class_name(o) : NULL;
            if (cn && strstr(cn, "JNISupport")) {
                struct posted *q = calloc(1, sizeof *q);
                snprintf(q->cls, sizeof q->cls, "%s", cn);
                q->ptr = jvm_field_get(o, jvm_field_id(cn, "__ptr", "J")).j;
                q->tag = jvm_field_get(o, jvm_field_id(cn, "__tag", "I")).i;
                snprintf(q->json, sizeof q->json,
                         "{\"errorCode\":0,\"errorText\":\"\",\"errorDetail\":\"\",\"requestTag\":%d,\"result\":%s}", q->tag,
                         nexon_result_payload(name, q->tag));
                wwd_logf("wwd-nexon", "%s.%s -> answer %s(peer %#llx, tag %d)", cls, name, cn, (unsigned long long)q->ptr, q->tag);
                pthread_mutex_lock(&g_post_mu);
                *g_post_tail = q;
                g_post_tail = &q->next;
                pthread_mutex_unlock(&g_post_mu);
            }
            p = strchr(p, ';') + 1;
        } else if (*p == '[') {
            while (*p == '[') p++;
            p = *p == 'L' ? strchr(p, ';') + 1 : p + 1;
        } else {
            p++;
        }
    }
}

static void deliver(JNIEnv *e, struct posted *q) {
    jclass k = jvm_find_class(q->cls);
    jstring js = jvm_new_string(q->json);
    const char *base = strrchr(q->cls, '/') + 1;
    void *f;
    if (!strcmp(base, "NXPListenerJNISupport") || !strcmp(base, "NXPToyResultListenerJNISupport") || !strcmp(base, "NPObdListenerJNISupport")) {
        if ((f = java_native(q->cls, "OnResult"))) ((void (*)(JNIEnv *, jclass, jlong, jint, jstring))f)(e, k, q->ptr, q->tag, js);
    } else if (!strcmp(base, "NPGCMListenerJNISupport") || !strcmp(base, "NXPDeviceNotificationStatusListenerJNISupport")) {
        if ((f = java_native(q->cls, "OnResult"))) ((void (*)(JNIEnv *, jclass, jlong, jint))f)(e, k, q->ptr, 0);
    } else if (!strcmp(base, "NXPPlateListenerJNISupport")) {
        if ((f = java_native(q->cls, "OnActionPerformed"))) ((void (*)(JNIEnv *, jclass, jlong, jint, jstring))f)(e, k, q->ptr, q->tag, js);
    } else if (!strcmp(base, "NPCloseListenerJNISupport") || !strcmp(base, "NXPTodayListenerJNISupport")) {
        if ((f = java_native(q->cls, "OnClose"))) ((void (*)(JNIEnv *, jclass, jlong, jstring))f)(e, k, q->ptr, js);
    } else if (strstr(base, "BannerListener")) {
        if ((f = java_native(q->cls, "OnDismissBanner"))) ((void (*)(JNIEnv *, jclass, jlong))f)(e, k, q->ptr);
    } else if (!strcmp(base, "NXPWebSchemeActionListenerJNISupport") || !strcmp(base, "NXPBillingOutOfAppPurchasesObserverJNISupport")) {
        f = (void *)1; // event sources, nothing to answer
    } else {
        if ((f = java_native(q->cls, "OnResult"))) ((void (*)(JNIEnv *, jclass, jlong, jstring))f)(e, k, q->ptr, js);
    }
    if (!f) wwd_logf("wwd-nexon", "no native callback for %s", q->cls);
}

// Font-path strings for the UI font repair (wwd fontfix.rs). They must come
// from Unreal's allocator (libUnreal's operator new = FMemory::Malloc): the
// repaired FFontData entries own them and Unreal frees them with FMemory.
static void font_path_pool(void) {
    static int done;
    if (done) return;
    void *(*unreal_new)(unsigned long) = (void *(*)(unsigned long))dlsym(RTLD_DEFAULT, "_Znwm");
    Dl_info info;
    if (!unreal_new || !dladdr((void *)unreal_new, &info) || !strstr(info.dli_fname, "libUnreal")) return;
    done = 1;
    static const char path[] = "/system/fonts/NotoSansCJK-Regular.ttc";
    for (int i = 0; i < 160; i++) {
        unsigned short *u = unreal_new(sizeof path * 2);
        for (size_t k = 0; k < sizeof path; k++) u[k] = (unsigned char)path[k];
    }
}

// GameControllerManager.scanDevices(): report our input devices to UE, which
// drops touches from devices it has not been told about.
static int g_report_devices;
static jvalue create_gcm(JNIEnv *e, jobject s, const jvalue *a) {
    g_report_devices = 1;
    return Z(1);
}
static void report_input_devices(JNIEnv *env) {
    if (!g_report_devices) return;
    g_report_devices = 0;
    void (*f)(JNIEnv *, jclass, jint, jint, jint) =
        (void (*)(JNIEnv *, jclass, jint, jint, jint))java_native("com/epicgames/unreal/GameControllerManager", "nativeOnInputDeviceStateEvent");
    if (!f) {
        wwd_logf("wwd-input", "nativeOnInputDeviceStateEvent not registered");
        return;
    }
    jclass k = jvm_find_class("com/epicgames/unreal/GameControllerManager");
    f(env, k, 1, 0 /* Added */, 1 /* TouchScreen */);
    f(env, k, 2, 0 /* Added */, 0 /* Unknown: keyboard */);
    wwd_logf("wwd-input", "reported touchscreen (id 1) and keyboard (id 2) to UE");
}

void java_pump(JNIEnv *env) {
    report_input_devices(env);
    if (!getenv("WWD_NO_FONT_POOL")) font_path_pool();
    char steps[64][48];
    pthread_mutex_lock(&g_patch_mu);
    int n = g_patch_n;
    memcpy(steps, g_patch_step, sizeof steps);
    g_patch_n = 0;
    pthread_mutex_unlock(&g_patch_mu);
    for (int i = 0; i < n; i++) { patcher_answer(env, steps[i]); wwd_logf("wwd-nexon", "patcher %s done", steps[i]); }
    pthread_mutex_lock(&g_post_mu);
    struct posted *q = g_post_head;
    g_post_head = NULL;
    g_post_tail = &g_post_head;
    pthread_mutex_unlock(&g_post_mu);
    while (q) {
        struct posted *n = q->next;
        wwd_logf("wwd-nexon", "deliver %s tag %d ...", q->cls, q->tag);
        deliver(env, q);
        wwd_logf("wwd-nexon", "deliver %s tag %d done", q->cls, q->tag);
        free(q);
        q = n;
    }
}
