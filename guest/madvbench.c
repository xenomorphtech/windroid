// Reproduce Scudo's mapping pattern and time the pieces.
#include <stdio.h>
#include <sys/mman.h>
#include <time.h>

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

int main(void) {
    double t0 = now();
    char *arena = mmap(0, 8ul << 30, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    double t1 = now();
    // carve 64 regions of 256 KiB RW with MAP_FIXED like the allocator does
    for (int i = 0; i < 64; i++) mmap(arena + (size_t)i * (128 << 20), 256 << 10, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    double t2 = now();
    for (int i = 0; i < 64; i++) arena[(size_t)i * (128 << 20) + 100] = 1;
    double t3 = now();
    for (int i = 0; i < 200; i++) madvise(arena + (size_t)(i % 64) * (128 << 20) + 4096 * (i % 32), 4096, MADV_DONTNEED);
    double t4 = now();
    printf("reserve 8G: %.3f ms\nmap fixed x64: %.3f ms\ntouch: %.3f ms\nmadvise x200: %.3f ms (%.1f us each)\n",
           (t1 - t0) * 1e3, (t2 - t1) * 1e3, (t3 - t2) * 1e3, (t4 - t3) * 1e3, (t4 - t3) * 1e6 / 200);
    return 0;
}
