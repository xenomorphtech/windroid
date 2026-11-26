// Self-checking test suite for the wwd runtime. Prints one line per check;
// run under both --interp and the JIT and diff the output.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <math.h>
#include <setjmp.h>
#include <pthread.h>
#include <stdatomic.h>
#include <time.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/utsname.h>

static int fails = 0, passes = 0;
#define CHECK(name, cond) do { if (cond) { passes++; printf("ok   %s\n", name); } \
    else { fails++; printf("FAIL %s  (line %d)\n", name, __LINE__); } } while (0)

static volatile uint64_t sink;

__attribute__((noinline)) static uint64_t mix(uint64_t x) {
    x ^= x >> 33; x *= 0xff51afd7ed558ccdULL;
    x ^= x >> 33; x *= 0xc4ceb9fe1a85ec53ULL;
    x ^= x >> 33; return x;
}

static int cmp_int(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

static jmp_buf jb;
__attribute__((noinline)) static void jumper(int v) { longjmp(jb, v); }

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static long shared_counter = 0;
static atomic_long atomic_counter = 0;

static void *worker(void *arg) {
    long n = (long)arg;
    for (long i = 0; i < n; i++) {
        pthread_mutex_lock(&mtx);
        shared_counter++;
        pthread_mutex_unlock(&mtx);
        atomic_fetch_add(&atomic_counter, 1);
    }
    return (void *)(n * 2);
}

int main(void) {
    // ---- integer arithmetic ----
    volatile int64_t a = -1234567890123LL, b = 987654321;
    CHECK("int64 div", a / b == -1249);
    CHECK("int64 mod", a % b == -(1234567890123LL - 1249LL * 987654321LL));
    volatile uint64_t u = 0xfedcba9876543210ULL;
    CHECK("umulh", (uint64_t)(((__uint128_t)u * u) >> 64) == 0xfdbac097c8dc5accULL);
    CHECK("clz/ctz", __builtin_clzll(1ULL << 40) == 23 && __builtin_ctzll(1ULL << 40) == 40);
    CHECK("popcount", __builtin_popcountll(u) == 32);
    CHECK("bswap", __builtin_bswap64(u) == 0x1032547698badcfeULL);
    volatile int32_t s32 = -7;
    volatile uint32_t u32 = 0x80000000u;
    CHECK("int32 ops", (s32 >> 1) == -4 && (u32 >> 31) == 1 && (int32_t)u32 < 0);
    uint64_t h = 0;
    for (int i = 0; i < 1000; i++) h += mix(i);
    sink = h;
    printf("     mix checksum %016llx\n", (unsigned long long)h);

    // ---- strings & memory ----
    char buf[256], buf2[256];
    for (int i = 0; i < 256; i++) buf[i] = (char)i;
    memcpy(buf2, buf, 256);
    CHECK("memcpy", memcmp(buf, buf2, 256) == 0);
    memmove(buf2 + 1, buf2, 100);
    CHECK("memmove", buf2[1] == 0 && buf2[100] == 99);
    memset(buf, 'x', 200);
    buf[200] = 0;
    CHECK("memset/strlen", strlen(buf) == 200);
    CHECK("strcmp", strcmp("apple", "apricot") < 0 && strcmp("same", "same") == 0);
    const char *hello = "hello world";
    CHECK("strchr/strstr", strchr(hello, 'w') == hello + 6 && strstr(hello, "orl") == hello + 7);
    char *dup = strdup("wwd runtime");
    CHECK("strdup", dup && strcmp(dup, "wwd runtime") == 0);
    free(dup);
    snprintf(buf, sizeof buf, "%d %u %x %ld %s %c %5.2f %e", -42, 42u, 0xbeef, 1234567890123L, "str", 'Z', 3.14159, 1e-10);
    CHECK("snprintf", strcmp(buf, "-42 42 beef 1234567890123 str Z  3.14 1.000000e-10") == 0);
    printf("     snprintf -> \"%s\"\n", buf);

    // ---- heap ----
    void *ptrs[500];
    for (int i = 0; i < 500; i++) {
        size_t sz = (mix(i) % 5000) + 1;
        ptrs[i] = malloc(sz);
        memset(ptrs[i], i, sz);
    }
    int heap_ok = 1;
    for (int i = 0; i < 500; i += 2) free(ptrs[i]);
    for (int i = 1; i < 500; i += 2)
        if (((unsigned char *)ptrs[i])[0] != (unsigned char)i) heap_ok = 0;
    for (int i = 1; i < 500; i += 2) free(ptrs[i]);
    char *big = malloc(64 << 20);
    memset(big, 1, 64 << 20);
    heap_ok &= big[(64 << 20) - 1] == 1;
    free(big);
    CHECK("malloc/free", heap_ok);
    int arr[1000];
    for (int i = 0; i < 1000; i++) arr[i] = (int)(mix(i) & 0xffff);
    qsort(arr, 1000, sizeof(int), cmp_int);
    int sorted = 1;
    for (int i = 1; i < 1000; i++)
        if (arr[i - 1] > arr[i]) sorted = 0;
    CHECK("qsort", sorted);

    // ---- floating point ----
    volatile double x = 2.0, y = 0.1;
    CHECK("sqrt", fabs(sqrt(x) - 1.41421356237) < 1e-9);
    CHECK("sin/cos", fabs(sin(1.0 * x / 2) - 0.8414709848) < 1e-9 && fabs(cos(x / 2) - 0.5403023059) < 1e-9);
    CHECK("exp/log", fabs(log(exp(3.5 * x / 2)) - 3.5) < 1e-12);
    CHECK("pow", fabs(pow(x, 10) - 1024.0) < 1e-12);
    double acc = 0;
    for (int i = 0; i < 10; i++) acc += y;
    CHECK("fp accumulate", fabs(acc - 1.0) < 1e-15);
    volatile float f = 1.5f;
    CHECK("float ops", f * 2.0f == 3.0f && (int)(f * 3.0f) == 4);
    volatile double neg = -2.7, pos = 3.99, half = 2.5;
    CHECK("fp->int", (int64_t)neg == -2 && (uint32_t)pos == 3 && lround(half) == 3);
    CHECK("strtod", fabs(strtod("6.02214076e23", NULL) - 6.02214076e23) < 1e9);
    printf("     pi = %.15f\n", 4.0 * atan(1.0 * x / 2));

    // ---- control flow ----
    int r = setjmp(jb);
    if (r == 0) jumper(7);
    CHECK("setjmp/longjmp", r == 7);

    // ---- syscalls ----
    struct utsname un;
    uname(&un);
    CHECK("uname", strcmp(un.machine, "aarch64") == 0);
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    struct timespec sl = {0, 20 * 1000 * 1000};
    nanosleep(&sl, NULL);
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long ms = (t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    CHECK("clock/nanosleep", ms >= 15 && ms < 1000);
    CHECK("time()", time(NULL) > 1700000000);
    CHECK("getpid", getpid() > 0);

    const char *tmp = "wwd_test_file.tmp";
    int fd = open(tmp, O_CREAT | O_TRUNC | O_RDWR, 0644);
    CHECK("open", fd >= 0);
    CHECK("write", write(fd, "hello file\n", 11) == 11);
    lseek(fd, 0, SEEK_SET);
    char rb[32] = {0};
    CHECK("read back", read(fd, rb, sizeof rb) == 11 && strcmp(rb, "hello file\n") == 0);
    struct stat st;
    fstat(fd, &st);
    CHECK("fstat", st.st_size == 11 && S_ISREG(st.st_mode));
    close(fd);
    FILE *fp = fopen(tmp, "r");
    char line[64] = {0};
    CHECK("stdio fopen/fgets", fp && fgets(line, sizeof line, fp) && strcmp(line, "hello file\n") == 0);
    if (fp) fclose(fp);
    CHECK("unlink", unlink(tmp) == 0 && access(tmp, F_OK) != 0);

    char *m = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK("mmap anon", m != MAP_FAILED && m[12345] == 0);
    m[0] = 1;
    m[(1 << 20) - 1] = 2;
    CHECK("munmap", munmap(m, 1 << 20) == 0);

    // ---- threads ----
    pthread_t th[4];
    void *ret[4];
    for (long i = 0; i < 4; i++) pthread_create(&th[i], NULL, worker, (void *)20000);
    for (int i = 0; i < 4; i++) pthread_join(th[i], &ret[i]);
    CHECK("pthread join value", (long)ret[0] == 40000 && (long)ret[3] == 40000);
    CHECK("mutex counter", shared_counter == 80000);
    CHECK("atomic counter", atomic_load(&atomic_counter) == 80000);

    printf("\n%d passed, %d failed\n", passes, fails);
    return fails ? 1 : 0;
}
