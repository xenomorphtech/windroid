// wwd fake JVM: enough of Java for native Android code (JNI) to run.
#pragma once
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include "include/jni.h"

enum { OBJ_INSTANCE = 1, OBJ_STRING, OBJ_ARRAY, OBJ_CLASS };

struct jclass_impl;
struct field_val;

typedef struct jobj {
    uint32_t magic; // 'JOBJ'
    int kind;
    struct jclass_impl *cls;
    // instance fields / native payload
    struct field_val *fields;
    void *native; // e.g. AAssetManager* for an AssetManager object
    // string
    char *utf8;
    jchar *utf16;
    jsize utf16_len;
    // array
    char elem; // z b c s i j f d l
    jsize len;
    void *data;
    // class object
    struct jclass_impl *self_class;
} jobj;

typedef jvalue (*java_fn)(JNIEnv *env, jobject self, const jvalue *args);

typedef struct jmethod {
    struct jclass_impl *cls;
    char *name;
    char *sig;
    int is_static;
    java_fn impl;      // our implementation (Java side)
    void *native;      // RegisterNatives'd native function
    struct jmethod *next;
} jmethod;

typedef struct jfield {
    struct jclass_impl *cls;
    char *name;
    char *sig;
    int is_static;
    jvalue static_val;
    struct jfield *next;
} jfield;

struct field_val {
    jfield *f;
    jvalue v;
    struct field_val *next;
};

typedef struct jclass_impl {
    char *name; // internal form: java/lang/String
    jobj *obj;  // the jclass object
    jmethod *methods;
    jfield *fields;
    struct jclass_impl *next;
} jclass_impl;

extern JavaVM *wwd_vm;
extern JNIEnv *wwd_env;

void jvm_init(void);
jclass jvm_find_class(const char *name);
jclass_impl *jvm_class_impl(const char *name);
jobject jvm_new_instance(const char *class_name);
jstring jvm_new_string(const char *utf8);
const char *jvm_string_utf8(jobject s);
jarray jvm_new_array(char elem, jsize len);
void *jvm_array_data(jarray a);
jsize jvm_array_len(jarray a);

/// Register a Java method implementation: "class.name(sig)".
void jvm_register(const char *cls, const char *name, const char *sig, java_fn fn, int is_static);
jfieldID jvm_field_id(const char *cls, const char *name, const char *sig);
/// Native implementation registered (RegisterNatives) for cls.name, any signature.
void *jvm_find_native(const char *cls, const char *name);
/// Register a static field value.
void jvm_static_field(const char *cls, const char *name, const char *sig, jvalue v);

// core used by the generated tables
void jvm_args_from_va(jmethodID m, va_list ap, jvalue *out);
jvalue jvm_invoke(JNIEnv *env, jobject obj, jclass cls, jmethodID m, const jvalue *args);
jvalue jvm_field_get(jobject o, jfieldID f);
void jvm_field_set(jobject o, jfieldID f, jvalue v);
jobject jvm_new_object(JNIEnv *env, jclass k, jmethodID ctor, const jvalue *args);

// logging
void wwd_logf(const char *tag, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
extern int wwd_jni_trace;

// java implementations (java_impl.c)
void java_register_all(void);
const char *jvm_object_class_name(jobject o);
// called for every unimplemented Java method (java_impl.c)
void java_unimplemented_hook(JNIEnv *env, const char *cls, const char *name, const char *sig, const jvalue *args);
// run callbacks posted to the "UI thread" (call from the launcher loop)
void java_pump(JNIEnv *env);
extern char wwd_cmdline[4096];
extern char wwd_pkg[256];
extern char wwd_pkg_dir[512];
