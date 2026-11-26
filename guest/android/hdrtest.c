#include <stdio.h>
#include <pthread.h>
#include <poll.h>
#include <sys/system_properties.h>
#include <android/api-level.h>
static pthread_mutex_t m = PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP;
int main(void) {
    char v[PROP_VALUE_MAX];
    __system_property_get("ro.build.version.release", v);
    pthread_mutex_lock(&m); pthread_mutex_lock(&m);
    printf("android %s api %d sizeof(pthread_mutex_t)=%zu FILE*=%p\n", v, android_get_device_api_level(), sizeof(pthread_mutex_t), (void*)stdout);
    return 0;
}
