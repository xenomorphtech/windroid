// wwd fake JVM core: objects, classes, method/field lookup, invocation.
#include "jvm.h"
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include "wwd_host.h"

#define JOBJ_MAGIC 0x4a4f424au

JavaVM *wwd_vm;
JNIEnv *wwd_env;
int wwd_jni_trace;
static pthread_mutex_t g_lock = PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP;
static jclass_impl *g_classes;

extern void *wwd_jni_table[WWD_JNI_SLOTS];
void wwd_jni_fill_generated(void);
int wwd_jni_slot(const char *name);
const char *wwd_jni_slot_name(int i);

void wwd_logf(const char *tag, const char *fmt, ...) {
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    wwd_host_log(4, tag, buf);
}

static char *xstrdup(const char *s) {
    size_t n = strlen(s) + 1;
    char *p = malloc(n);
    memcpy(p, s, n);
    return p;
}

static jobj *new_obj(int kind, jclass_impl *cls) {
    jobj *o = calloc(1, sizeof(jobj));
    o->magic = JOBJ_MAGIC;
    o->kind = kind;
    o->cls = cls;
    return o;
}

static jobj *as_obj(jobject o) {
    jobj *p = (jobj *)o;
    if (!p) return NULL;
    if (p->magic != JOBJ_MAGIC) {
        wwd_logf("wwd-jvm", "not a jobject: %p", o);
        return NULL;
    }
    return p;
}

// ---------------------------------------------------------------------------
// classes
// ---------------------------------------------------------------------------

static void normalize(char *s) {
    for (; *s; s++)
        if (*s == '.') *s = '/';
}

jclass_impl *jvm_class_impl(const char *name_in) {
    char name[512];
    snprintf(name, sizeof name, "%s", name_in);
    normalize(name);
    // "Lfoo/Bar;" -> "foo/Bar"
    size_t n = strlen(name);
    if (name[0] == 'L' && n > 2 && name[n - 1] == ';') {
        memmove(name, name + 1, n - 2);
        name[n - 2] = 0;
    }
    pthread_mutex_lock(&g_lock);
    for (jclass_impl *c = g_classes; c; c = c->next)
        if (!strcmp(c->name, name)) {
            pthread_mutex_unlock(&g_lock);
            return c;
        }
    jclass_impl *c = calloc(1, sizeof *c);
    c->name = xstrdup(name);
    c->obj = new_obj(OBJ_CLASS, NULL);
    c->obj->self_class = c;
    c->next = g_classes;
    g_classes = c;
    pthread_mutex_unlock(&g_lock);
    // java/lang/Class objects are instances of java/lang/Class
    if (strcmp(name, "java/lang/Class")) c->obj->cls = jvm_class_impl("java/lang/Class");
    return c;
}

jclass jvm_find_class(const char *name) { return jvm_class_impl(name)->obj; }

static jclass_impl *class_of(jobject k) {
    jobj *o = as_obj(k);
    if (!o) return NULL;
    return o->kind == OBJ_CLASS ? o->self_class : o->cls;
}

const char *jvm_object_class_name(jobject o) {
    jclass_impl *c = class_of(o);
    return c ? c->name : NULL;
}

static jmethod *find_method(jclass_impl *c, const char *name, const char *sig, int is_static, int create) {
    pthread_mutex_lock(&g_lock);
    for (jmethod *m = c->methods; m; m = m->next)
        if (!strcmp(m->name, name) && !strcmp(m->sig, sig)) {
            pthread_mutex_unlock(&g_lock);
            return m;
        }
    jmethod *m = NULL;
    if (create) {
        m = calloc(1, sizeof *m);
        m->cls = c;
        m->name = xstrdup(name);
        m->sig = xstrdup(sig);
        m->is_static = is_static;
        m->next = c->methods;
        c->methods = m;
    }
    pthread_mutex_unlock(&g_lock);
    return m;
}

static jfield *find_field(jclass_impl *c, const char *name, const char *sig, int is_static) {
    pthread_mutex_lock(&g_lock);
    for (jfield *f = c->fields; f; f = f->next)
        if (!strcmp(f->name, name) && !strcmp(f->sig, sig)) {
            pthread_mutex_unlock(&g_lock);
            return f;
        }
    jfield *f = calloc(1, sizeof *f);
    f->cls = c;
    f->name = xstrdup(name);
    f->sig = xstrdup(sig);
    f->is_static = is_static;
    f->next = c->fields;
    c->fields = f;
    pthread_mutex_unlock(&g_lock);
    return f;
}

void jvm_register(const char *cls, const char *name, const char *sig, java_fn fn, int is_static) {
    jmethod *m = find_method(jvm_class_impl(cls), name, sig, is_static, 1);
    m->impl = fn;
    m->is_static = is_static;
}

jfieldID jvm_field_id(const char *cls, const char *name, const char *sig) { return (jfieldID)find_field(jvm_class_impl(cls), name, sig, 0); }

void *jvm_find_native(const char *cls, const char *name) {
    jclass_impl *c = jvm_class_impl(cls);
    pthread_mutex_lock(&g_lock);
    void *r = NULL;
    for (jmethod *m = c->methods; m; m = m->next)
        if (m->native && !strcmp(m->name, name)) {
            r = m->native;
            break;
        }
    pthread_mutex_unlock(&g_lock);
    return r;
}

void jvm_static_field(const char *cls, const char *name, const char *sig, jvalue v) {
    jfield *f = find_field(jvm_class_impl(cls), name, sig, 1);
    f->static_val = v;
}

// ---------------------------------------------------------------------------
// objects, strings, arrays
// ---------------------------------------------------------------------------

jobject jvm_new_instance(const char *class_name) { return new_obj(OBJ_INSTANCE, jvm_class_impl(class_name)); }

jstring jvm_new_string(const char *utf8) {
    if (!utf8) return NULL;
    static jclass_impl *str_cls;
    if (!str_cls) str_cls = jvm_class_impl("java/lang/String");
    jobj *o = new_obj(OBJ_STRING, str_cls);
    o->utf8 = xstrdup(utf8);
    return o;
}

const char *jvm_string_utf8(jobject s) {
    jobj *o = as_obj(s);
    return (o && o->kind == OBJ_STRING) ? o->utf8 : NULL;
}

static void ensure_utf16(jobj *o) {
    if (o->utf16) return;
    size_t n = strlen(o->utf8);
    o->utf16 = calloc(n + 1, sizeof(jchar));
    const unsigned char *p = (const unsigned char *)o->utf8;
    jsize k = 0;
    while (*p) {
        uint32_t cp;
        if (*p < 0x80) cp = *p++;
        else if ((*p & 0xe0) == 0xc0) { cp = (*p++ & 0x1f) << 6; cp |= (*p++ & 0x3f); }
        else if ((*p & 0xf0) == 0xe0) { cp = (*p++ & 0x0f) << 12; cp |= (*p++ & 0x3f) << 6; cp |= (*p++ & 0x3f); }
        else { cp = (*p++ & 0x07) << 18; cp |= (*p++ & 0x3f) << 12; cp |= (*p++ & 0x3f) << 6; cp |= (*p++ & 0x3f); }
        if (cp >= 0x10000) {
            cp -= 0x10000;
            o->utf16[k++] = 0xd800 + (cp >> 10);
            o->utf16[k++] = 0xdc00 + (cp & 0x3ff);
        } else {
            o->utf16[k++] = (jchar)cp;
        }
    }
    o->utf16_len = k;
}

static jstring string_from_utf16(const jchar *s, jsize n) {
    char *buf = malloc((size_t)n * 3 + 1), *q = buf;
    for (jsize i = 0; i < n; i++) {
        uint32_t c = s[i];
        if (c >= 0xd800 && c < 0xdc00 && i + 1 < n) {
            c = 0x10000 + ((c - 0xd800) << 10) + (s[++i] - 0xdc00);
        }
        if (c < 0x80) *q++ = (char)c;
        else if (c < 0x800) { *q++ = 0xc0 | (c >> 6); *q++ = 0x80 | (c & 0x3f); }
        else if (c < 0x10000) { *q++ = 0xe0 | (c >> 12); *q++ = 0x80 | ((c >> 6) & 0x3f); *q++ = 0x80 | (c & 0x3f); }
        else { *q++ = 0xf0 | (c >> 18); *q++ = 0x80 | ((c >> 12) & 0x3f); *q++ = 0x80 | ((c >> 6) & 0x3f); *q++ = 0x80 | (c & 0x3f); }
    }
    *q = 0;
    jstring r = jvm_new_string(buf);
    free(buf);
    return r;
}

static size_t elem_size(char e) {
    switch (e) {
    case 'z': case 'b': return 1;
    case 'c': case 's': return 2;
    case 'i': case 'f': return 4;
    default: return 8;
    }
}

jarray jvm_new_array(char elem, jsize len) {
    char cname[8] = "[";
    cname[1] = elem == 'l' ? 'L' : (char)(elem - 32);
    jobj *o = new_obj(OBJ_ARRAY, jvm_class_impl(cname));
    o->elem = elem;
    o->len = len;
    o->data = calloc((size_t)len + 1, elem_size(elem));
    return o;
}

void *jvm_array_data(jarray a) {
    jobj *o = as_obj(a);
    return o ? o->data : NULL;
}

jsize jvm_array_len(jarray a) {
    jobj *o = as_obj(a);
    return o ? o->len : 0;
}

// ---------------------------------------------------------------------------
// invocation
// ---------------------------------------------------------------------------

void jvm_args_from_va(jmethodID mid, va_list ap, jvalue *out) {
    jmethod *m = (jmethod *)mid;
    const char *p = m ? strchr(m->sig, '(') : NULL;
    if (!p) return;
    p++;
    int i = 0;
    while (*p && *p != ')' && i < 32) {
        switch (*p) {
        case 'Z': case 'B': case 'C': case 'S': case 'I': out[i].j = 0; out[i].i = va_arg(ap, jint); p++; break;
        case 'J': out[i].j = va_arg(ap, jlong); p++; break;
        case 'F': out[i].j = 0; out[i].f = (jfloat)va_arg(ap, double); p++; break;
        case 'D': out[i].d = va_arg(ap, jdouble); p++; break;
        case 'L': out[i].l = va_arg(ap, jobject); p = strchr(p, ';') + 1; break;
        case '[':
            while (*p == '[') p++;
            if (*p == 'L') p = strchr(p, ';') + 1; else p++;
            out[i].l = va_arg(ap, jobject);
            break;
        default: return;
        }
        i++;
    }
}

static char ret_kind(const char *sig) {
    const char *r = strchr(sig, ')');
    return r ? r[1] : 'V';
}

jvalue jvm_invoke(JNIEnv *env, jobject obj, jclass cls, jmethodID mid, const jvalue *args) {
    jvalue r;
    r.j = 0;
    jmethod *m = (jmethod *)mid;
    if (!m) {
        wwd_logf("wwd-jvm", "call through NULL jmethodID");
        return r;
    }
    // virtual dispatch: an implementation registered on the receiver's own class wins
    if (obj && !m->is_static) {
        jclass_impl *rc = class_of(obj);
        if (rc && rc != m->cls) {
            jmethod *o = find_method(rc, m->name, m->sig, 0, 0);
            if (o && o->impl) m = o;
        }
    }
    // show string arguments: they carry the interesting payload (dialog text, keys...)
    char argdesc[1024] = "";
    {
        const char *p = strchr(m->sig, '(') + 1;
        int i = 0;
        size_t o = 0;
        while (*p && *p != ')' && i < 32 && o < sizeof argdesc - 64) {
            if (!strncmp(p, "Ljava/lang/String;", 18)) {
                const char *v = jvm_string_utf8(args[i].l);
                o += (size_t)snprintf(argdesc + o, sizeof argdesc - o, " \"%.400s\"", v ? v : "(null)");
            }
            if (*p == 'L') p = strchr(p, ';') + 1;
            else if (*p == '[') { while (*p == '[') p++; if (*p == 'L') p = strchr(p, ';') + 1; else p++; }
            else p++;
            i++;
        }
    }
    if (wwd_jni_trace) wwd_logf("wwd-jni", "call %s.%s%s%s", m->cls->name, m->name, m->sig, argdesc);
    if (m->impl) return m->impl(env, obj, args);
    (void)cls;
    char rk = ret_kind(m->sig);
    java_unimplemented_hook(env, m->cls->name, m->name, m->sig, args);
    wwd_logf("wwd-jvm", "UNIMPLEMENTED %s%s.%s%s ->%s", m->is_static ? "static " : "", m->cls->name, m->name, m->sig, argdesc);
    // safe-ish defaults: empty strings rather than null, false/0 otherwise
    if (rk == 'L' && !strncmp(strchr(m->sig, ')') + 1, "Ljava/lang/String;", 18)) r.l = jvm_new_string("");
    return r;
}

jvalue jvm_field_get(jobject o, jfieldID fid) {
    jfield *f = (jfield *)fid;
    jvalue v;
    v.j = 0;
    if (!f) return v;
    if (f->is_static) return f->static_val;
    jobj *p = as_obj(o);
    if (!p) return v;
    for (struct field_val *fv = p->fields; fv; fv = fv->next)
        if (fv->f == f) return fv->v;
    if (wwd_jni_trace) wwd_logf("wwd-jvm", "unset field %s.%s:%s", f->cls->name, f->name, f->sig);
    return v;
}

void jvm_field_set(jobject o, jfieldID fid, jvalue v) {
    jfield *f = (jfield *)fid;
    if (!f) return;
    if (f->is_static) {
        f->static_val = v;
        return;
    }
    jobj *p = as_obj(o);
    if (!p) return;
    for (struct field_val *fv = p->fields; fv; fv = fv->next)
        if (fv->f == f) {
            fv->v = v;
            return;
        }
    struct field_val *fv = calloc(1, sizeof *fv);
    fv->f = f;
    fv->v = v;
    fv->next = p->fields;
    p->fields = fv;
}

jobject jvm_new_object(JNIEnv *env, jclass k, jmethodID ctor, const jvalue *args) {
    jclass_impl *c = class_of(k);
    jobj *o = new_obj(OBJ_INSTANCE, c);
    jmethod *m = (jmethod *)ctor;
    if (m && m->impl) m->impl(env, o, args);
    else if (m && !strncmp(m->sig, "(J", 2)) {
        // native-peer objects (listeners): remember the peer pointer and tag
        jvm_field_set(o, jvm_field_id(c->name, "__ptr", "J"), (jvalue){.j = args[0].j});
        if (m->sig[2] == 'I') jvm_field_set(o, jvm_field_id(c->name, "__tag", "I"), (jvalue){.i = args[1].i});
        if (wwd_jni_trace) wwd_logf("wwd-jvm", "new %s%s peer=%#llx", c->name, m->sig, (unsigned long long)args[0].j);
    } else if (wwd_jni_trace && m) wwd_logf("wwd-jvm", "new %s%s (no ctor impl)", c->name, m->sig);
    return o;
}

// ---------------------------------------------------------------------------
// the hand-written part of the JNIEnv table
// ---------------------------------------------------------------------------

static jint j_GetVersion(JNIEnv *e) { (void)e; return JNI_VERSION_1_6; }
static jclass j_FindClass(JNIEnv *e, const char *n) { (void)e; if (wwd_jni_trace) wwd_logf("wwd-jni", "FindClass %s", n); return jvm_find_class(n); }
static jclass j_GetSuperclass(JNIEnv *e, jclass k) { (void)e; (void)k; return jvm_find_class("java/lang/Object"); }
static jboolean j_IsAssignableFrom(JNIEnv *e, jclass a, jclass b) { (void)e; (void)a; (void)b; return 1; }
static jint j_Throw(JNIEnv *e, jthrowable t) { (void)e; (void)t; return 0; }
static jint j_ThrowNew(JNIEnv *e, jclass k, const char *msg) { (void)e; wwd_logf("wwd-jvm", "ThrowNew %s: %s", class_of(k) ? class_of(k)->name : "?", msg ? msg : ""); return 0; }
static jthrowable j_ExceptionOccurred(JNIEnv *e) { (void)e; return NULL; }
static void j_ExceptionDescribe(JNIEnv *e) { (void)e; }
static void j_ExceptionClear(JNIEnv *e) { (void)e; }
static void j_FatalError(JNIEnv *e, const char *msg) { (void)e; wwd_logf("wwd-jvm", "FatalError: %s", msg); abort(); }
static jint j_PushLocalFrame(JNIEnv *e, jint n) { (void)e; (void)n; return 0; }
static jobject j_PopLocalFrame(JNIEnv *e, jobject r) { (void)e; return r; }
static jobject j_NewRef(JNIEnv *e, jobject o) { (void)e; return o; }
static void j_DeleteRef(JNIEnv *e, jobject o) { (void)e; (void)o; }
static jboolean j_IsSameObject(JNIEnv *e, jobject a, jobject b) { (void)e; return a == b; }
static jint j_EnsureLocalCapacity(JNIEnv *e, jint n) { (void)e; (void)n; return 0; }
static jobject j_AllocObject(JNIEnv *e, jclass k) { (void)e; return new_obj(OBJ_INSTANCE, class_of(k)); }
static jclass j_GetObjectClass(JNIEnv *e, jobject o) { (void)e; jobj *p = as_obj(o); return (p && p->cls) ? p->cls->obj : jvm_find_class("java/lang/Object"); }
static jboolean j_IsInstanceOf(JNIEnv *e, jobject o, jclass k) {
    (void)e;
    if (!o) return 1;
    jclass_impl *a = class_of(o), *b = class_of(k);
    return a == b || (b && !strcmp(b->name, "java/lang/Object"));
}
static jmethodID j_GetMethodID(JNIEnv *e, jclass k, const char *n, const char *s) {
    (void)e;
    jclass_impl *c = class_of(k);
    if (!c) return NULL;
    if (wwd_jni_trace) wwd_logf("wwd-jni", "GetMethodID %s.%s%s", c->name, n, s);
    return (jmethodID)find_method(c, n, s, 0, 1);
}
static jmethodID j_GetStaticMethodID(JNIEnv *e, jclass k, const char *n, const char *s) {
    (void)e;
    jclass_impl *c = class_of(k);
    if (!c) return NULL;
    if (wwd_jni_trace) wwd_logf("wwd-jni", "GetStaticMethodID %s.%s%s", c->name, n, s);
    jmethod *m = find_method(c, n, s, 1, 1);
    m->is_static = 1;
    return (jmethodID)m;
}
static jfieldID j_GetFieldID(JNIEnv *e, jclass k, const char *n, const char *s) { (void)e; jclass_impl *c = class_of(k); return c ? (jfieldID)find_field(c, n, s, 0) : NULL; }
static jfieldID j_GetStaticFieldID(JNIEnv *e, jclass k, const char *n, const char *s) {
    (void)e;
    jclass_impl *c = class_of(k);
    if (!c) return NULL;
    jfield *f = find_field(c, n, s, 1);
    f->is_static = 1;
    return (jfieldID)f;
}
static jstring j_NewString(JNIEnv *e, const jchar *s, jsize n) { (void)e; return string_from_utf16(s, n); }
static jsize j_GetStringLength(JNIEnv *e, jstring s) { (void)e; jobj *o = as_obj(s); if (!o) return 0; ensure_utf16(o); return o->utf16_len; }
static const jchar *j_GetStringChars(JNIEnv *e, jstring s, jboolean *c) { (void)e; if (c) *c = 0; jobj *o = as_obj(s); if (!o) return NULL; ensure_utf16(o); return o->utf16; }
static void j_ReleaseStringChars(JNIEnv *e, jstring s, const jchar *p) { (void)e; (void)s; (void)p; }
static jstring j_NewStringUTF(JNIEnv *e, const char *s) { (void)e; return jvm_new_string(s); }
static jsize j_GetStringUTFLength(JNIEnv *e, jstring s) { (void)e; const char *p = jvm_string_utf8(s); return p ? (jsize)strlen(p) : 0; }
static const char *j_GetStringUTFChars(JNIEnv *e, jstring s, jboolean *c) { (void)e; if (c) *c = 0; const char *p = jvm_string_utf8(s); return p ? p : ""; }
static void j_ReleaseStringUTFChars(JNIEnv *e, jstring s, const char *p) { (void)e; (void)s; (void)p; }
static jsize j_GetArrayLength(JNIEnv *e, jarray a) { (void)e; return jvm_array_len(a); }
static jobjectArray j_NewObjectArray(JNIEnv *e, jsize n, jclass k, jobject init) {
    (void)e; (void)k;
    jobj *a = jvm_new_array('l', n);
    for (jsize i = 0; i < n; i++) ((jobject *)a->data)[i] = init;
    return a;
}
static jobject j_GetObjectArrayElement(JNIEnv *e, jobjectArray a, jsize i) { (void)e; jobj *o = as_obj(a); return (o && i < o->len) ? ((jobject *)o->data)[i] : NULL; }
static void j_SetObjectArrayElement(JNIEnv *e, jobjectArray a, jsize i, jobject v) { (void)e; jobj *o = as_obj(a); if (o && i < o->len) ((jobject *)o->data)[i] = v; }
static jint j_RegisterNatives(JNIEnv *e, jclass k, const JNINativeMethod *ms, jint n) {
    (void)e;
    jclass_impl *c = class_of(k);
    for (jint i = 0; i < n; i++) {
        jmethod *m = find_method(c, ms[i].name, ms[i].signature, 0, 1);
        m->native = ms[i].fnPtr;
        if (wwd_jni_trace) wwd_logf("wwd-jni", "RegisterNatives %s.%s%s", c->name, ms[i].name, ms[i].signature);
    }
    return 0;
}
static jint j_UnregisterNatives(JNIEnv *e, jclass k) { (void)e; (void)k; return 0; }
static jint j_MonitorEnter(JNIEnv *e, jobject o) { (void)e; (void)o; pthread_mutex_lock(&g_lock); return 0; }
static jint j_MonitorExit(JNIEnv *e, jobject o) { (void)e; (void)o; pthread_mutex_unlock(&g_lock); return 0; }
static jint j_GetJavaVM(JNIEnv *e, JavaVM **vm) { (void)e; *vm = wwd_vm; return 0; }
static void j_GetStringRegion(JNIEnv *e, jstring s, jsize st, jsize n, jchar *buf) { (void)e; jobj *o = as_obj(s); if (!o) return; ensure_utf16(o); memcpy(buf, o->utf16 + st, (size_t)n * 2); }
static void j_GetStringUTFRegion(JNIEnv *e, jstring s, jsize st, jsize n, char *buf) { (void)e; const char *p = jvm_string_utf8(s); if (p) { memcpy(buf, p + st, (size_t)n); buf[n] = 0; } }
static void *j_GetPrimitiveArrayCritical(JNIEnv *e, jarray a, jboolean *c) { (void)e; if (c) *c = 0; return jvm_array_data(a); }
static void j_ReleasePrimitiveArrayCritical(JNIEnv *e, jarray a, void *p, jint m) { (void)e; (void)a; (void)p; (void)m; }
static const jchar *j_GetStringCritical(JNIEnv *e, jstring s, jboolean *c) { return j_GetStringChars(e, s, c); }
static void j_ReleaseStringCritical(JNIEnv *e, jstring s, const jchar *p) { (void)e; (void)s; (void)p; }
static jboolean j_ExceptionCheck(JNIEnv *e) { (void)e; return 0; }
static jobject j_NewDirectByteBuffer(JNIEnv *e, void *addr, jlong cap) {
    (void)e;
    jobj *o = new_obj(OBJ_INSTANCE, jvm_class_impl("java/nio/DirectByteBuffer"));
    o->data = addr;
    o->len = (jsize)cap;
    return o;
}
static void *j_GetDirectBufferAddress(JNIEnv *e, jobject b) { (void)e; jobj *o = as_obj(b); return o ? o->data : NULL; }
static jlong j_GetDirectBufferCapacity(JNIEnv *e, jobject b) { (void)e; jobj *o = as_obj(b); return o ? o->len : -1; }
static jobjectRefType j_GetObjectRefType(JNIEnv *e, jobject o) { (void)e; return o ? JNIGlobalRefType : JNIInvalidRefType; }
static jmethodID j_FromReflectedMethod(JNIEnv *e, jobject m) { (void)e; (void)m; return NULL; }
static jfieldID j_FromReflectedField(JNIEnv *e, jobject f) { (void)e; (void)f; return NULL; }
static jobject j_ToReflected(JNIEnv *e, jclass k, void *id, jboolean st) { (void)e; (void)k; (void)id; (void)st; return NULL; }
static jclass j_DefineClass(JNIEnv *e, const char *n, jobject l, const jbyte *b, jsize s) { (void)e; (void)l; (void)b; (void)s; return jvm_find_class(n); }

static void j_missing(void) {
    wwd_logf("wwd-jvm", "call to an unfilled JNI slot");
    abort();
}

// JavaVM
static struct JNINativeInterface g_env_table;
static const struct JNINativeInterface *g_env_ptr = &g_env_table;
static jint vm_Destroy(JavaVM *vm) { (void)vm; return 0; }
static jint vm_Attach(JavaVM *vm, JNIEnv **env, void *args) { (void)vm; (void)args; *env = wwd_env; return 0; }
static jint vm_Detach(JavaVM *vm) { (void)vm; return 0; }
static jint vm_GetEnv(JavaVM *vm, void **env, jint ver) { (void)vm; (void)ver; *env = wwd_env; return 0; }
static struct JNIInvokeInterface g_vm_table = {0, 0, 0, vm_Destroy, vm_Attach, vm_Detach, vm_GetEnv, vm_Attach};
static const struct JNIInvokeInterface *g_vm_ptr = &g_vm_table;

#define SET(name, fn) wwd_jni_table[wwd_jni_slot(#name)] = (void *)(fn)

void jvm_init(void) {
    if (wwd_vm) return;
    wwd_jni_fill_generated();
    SET(GetVersion, j_GetVersion);
    SET(DefineClass, j_DefineClass);
    SET(FindClass, j_FindClass);
    SET(FromReflectedMethod, j_FromReflectedMethod);
    SET(FromReflectedField, j_FromReflectedField);
    SET(ToReflectedMethod, j_ToReflected);
    SET(GetSuperclass, j_GetSuperclass);
    SET(IsAssignableFrom, j_IsAssignableFrom);
    SET(ToReflectedField, j_ToReflected);
    SET(Throw, j_Throw);
    SET(ThrowNew, j_ThrowNew);
    SET(ExceptionOccurred, j_ExceptionOccurred);
    SET(ExceptionDescribe, j_ExceptionDescribe);
    SET(ExceptionClear, j_ExceptionClear);
    SET(FatalError, j_FatalError);
    SET(PushLocalFrame, j_PushLocalFrame);
    SET(PopLocalFrame, j_PopLocalFrame);
    SET(NewGlobalRef, j_NewRef);
    SET(DeleteGlobalRef, j_DeleteRef);
    SET(DeleteLocalRef, j_DeleteRef);
    SET(IsSameObject, j_IsSameObject);
    SET(NewLocalRef, j_NewRef);
    SET(EnsureLocalCapacity, j_EnsureLocalCapacity);
    SET(AllocObject, j_AllocObject);
    SET(GetObjectClass, j_GetObjectClass);
    SET(IsInstanceOf, j_IsInstanceOf);
    SET(GetMethodID, j_GetMethodID);
    SET(GetFieldID, j_GetFieldID);
    SET(GetStaticMethodID, j_GetStaticMethodID);
    SET(GetStaticFieldID, j_GetStaticFieldID);
    SET(NewString, j_NewString);
    SET(GetStringLength, j_GetStringLength);
    SET(GetStringChars, j_GetStringChars);
    SET(ReleaseStringChars, j_ReleaseStringChars);
    SET(NewStringUTF, j_NewStringUTF);
    SET(GetStringUTFLength, j_GetStringUTFLength);
    SET(GetStringUTFChars, j_GetStringUTFChars);
    SET(ReleaseStringUTFChars, j_ReleaseStringUTFChars);
    SET(GetArrayLength, j_GetArrayLength);
    SET(NewObjectArray, j_NewObjectArray);
    SET(GetObjectArrayElement, j_GetObjectArrayElement);
    SET(SetObjectArrayElement, j_SetObjectArrayElement);
    SET(RegisterNatives, j_RegisterNatives);
    SET(UnregisterNatives, j_UnregisterNatives);
    SET(MonitorEnter, j_MonitorEnter);
    SET(MonitorExit, j_MonitorExit);
    SET(GetJavaVM, j_GetJavaVM);
    SET(GetStringRegion, j_GetStringRegion);
    SET(GetStringUTFRegion, j_GetStringUTFRegion);
    SET(GetPrimitiveArrayCritical, j_GetPrimitiveArrayCritical);
    SET(ReleasePrimitiveArrayCritical, j_ReleasePrimitiveArrayCritical);
    SET(GetStringCritical, j_GetStringCritical);
    SET(ReleaseStringCritical, j_ReleaseStringCritical);
    SET(NewWeakGlobalRef, j_NewRef);
    SET(DeleteWeakGlobalRef, j_DeleteRef);
    SET(ExceptionCheck, j_ExceptionCheck);
    SET(NewDirectByteBuffer, j_NewDirectByteBuffer);
    SET(GetDirectBufferAddress, j_GetDirectBufferAddress);
    SET(GetDirectBufferCapacity, j_GetDirectBufferCapacity);
    SET(GetObjectRefType, j_GetObjectRefType);
    void **dst = (void **)&g_env_table;
    for (int i = 0; i < WWD_JNI_SLOTS; i++) {
        dst[i] = wwd_jni_table[i];
        if (!dst[i] && i >= 4) {
            wwd_logf("wwd-jvm", "JNI slot %d (%s) unfilled", i, wwd_jni_slot_name(i));
            dst[i] = (void *)j_missing;
        }
    }
    wwd_env = (JNIEnv *)&g_env_ptr;
    wwd_vm = (JavaVM *)&g_vm_ptr;
    java_register_all();
}
