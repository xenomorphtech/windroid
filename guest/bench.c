// CPU-bound benchmark: sieve + integer hashing + floating-point loop.
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <time.h>

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

int main(int argc, char **argv) {
    int n = argc > 1 ? atoi(argv[1]) : 5000000;
    double t0 = now();
    char *sieve = calloc(n + 1, 1);
    int primes = 0;
    for (int i = 2; i <= n; i++)
        if (!sieve[i]) {
            primes++;
            for (long j = (long)i * i; j <= n; j += i) sieve[j] = 1;
        }
    free(sieve);
    double t1 = now();
    uint64_t h = 1469598103934665603ULL;
    for (int i = 0; i < n * 4; i++) {
        h ^= (uint64_t)i;
        h *= 1099511628211ULL;
        h = (h << 13) | (h >> 51);
    }
    double t2 = now();
    double x = 0.5, v = 0.0, sum = 0.0;
    for (int i = 0; i < n * 2; i++) {
        double acc = -x * 0.01;
        v += acc;
        x += v * 0.01;
        sum += x * x;
    }
    double t3 = now();
    printf("primes<=%d: %d   (%.3fs)\n", n, primes, t1 - t0);
    printf("hash: %016llx   (%.3fs)\n", (unsigned long long)h, t2 - t1);
    printf("fp: %.6f   (%.3fs)\n", sum, t3 - t2);
    printf("total %.3fs\n", t3 - t0);
    return 0;
}
