// Bionic smoke test: libc, libm, dlopen, pthreads, TLS.
typedef unsigned long size_t;
int printf(const char *, ...);
void *dlopen(const char *, int);
void *dlsym(void *, const char *);
const char *dlerror(void);
double sqrt(double);
int pthread_create(void *, const void *, void *(*)(void *), void *);
int pthread_join(unsigned long, void **);
int __system_property_get(const char *, char *);

static __thread int tls_value __attribute__((aligned(64))) = 41;

static void *worker(void *arg) {
    tls_value += (int)(long)arg;
    return (void *)(long)tls_value;
}

int main(int argc, char **argv, char **envp) {
    (void)envp;
    printf("bionic hello: argc=%d argv[0]=%s\n", argc, argv[0]);
    volatile double two = 2.0;
    printf("libm sqrt(2)=%.9f\n", sqrt(two));
    void *h = dlopen("libm.so", 2);
    double (*cbrt_)(double) = h ? (double (*)(double))dlsym(h, "cbrt") : 0;
    printf("dlopen libm=%p cbrt(27)=%.3f %s\n", h, cbrt_ ? cbrt_(27.0) : -1.0, h ? "" : dlerror());
    unsigned long th;
    void *ret;
    pthread_create(&th, 0, worker, (void *)1);
    pthread_join(th, &ret);
    printf("thread tls result=%ld main tls=%d\n", (long)ret, tls_value);
    char sdk[92] = "?";
    __system_property_get("ro.build.version.sdk", sdk);
    printf("ro.build.version.sdk=%s\n", sdk);
    return 0;
}
