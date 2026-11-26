// Signal delivery tests: the patterns ART and crash handlers depend on.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ucontext.h>
#include <unistd.h>

static int fails = 0, passes = 0;
#define CHECK(name, cond) do { if (cond) { passes++; printf("ok   %s\n", name); } \
    else { fails++; printf("FAIL %s  (line %d)\n", name, __LINE__); } } while (0)

static volatile int got_usr1 = 0, usr1_code = 0;
static void on_usr1(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    got_usr1 = sig;
    usr1_code = si->si_code;
}

// ART-style implicit null check: the handler skips the faulting load and
// sets x0 to a marker so the code continues.
static volatile uintptr_t fault_addr = 0;
static void on_segv_skip(int sig, siginfo_t *si, void *ucv) {
    (void)sig;
    ucontext_t *uc = ucv;
    fault_addr = (uintptr_t)si->si_addr;
    uc->uc_mcontext.pc += 4;
    uc->uc_mcontext.regs[0] = 0x5afe;
}

__attribute__((noinline)) static uint64_t load_from(uint64_t *p) {
    uint64_t v;
    __asm__ volatile("ldr %0, [%1]" : "=r"(v) : "r"(p) : "memory");
    return v;
}

static sigjmp_buf env;
static void on_segv_jump(int sig) { siglongjmp(env, sig); }

static volatile int on_alt = 0;
static char altstack[64 * 1024];
static void on_usr2(int sig) {
    (void)sig;
    char probe;
    on_alt = (&probe >= altstack && &probe < altstack + sizeof altstack);
}

__attribute__((noinline)) static long hot_loop(long *p, long n) {
    long s = 0;
    for (long i = 0; i < n; i++) s += p[i & 7];
    return s;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR1, &sa, NULL);
    raise(SIGUSR1);
    CHECK("raise(SIGUSR1) runs handler", got_usr1 == SIGUSR1);
    CHECK("si_code is SI_TKILL", usr1_code == SI_TKILL);

    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv_skip;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    uint64_t r = load_from((uint64_t *)0x10);
    CHECK("SIGSEGV handler skips faulting insn", r == 0x5afe);
    CHECK("si_addr is fault address", fault_addr == 0x10);

    signal(SIGSEGV, on_segv_jump);
    int j = sigsetjmp(env, 1);
    if (j == 0) {
        volatile int *bad = (int *)(uintptr_t)0xdead0000;
        *bad = 1;
    }
    CHECK("siglongjmp out of SIGSEGV handler", j == SIGSEGV);

    // fault inside a hot (JIT-compiled) loop, recovered by longjmp again
    long data[8] = {1, 2, 3, 4, 5, 6, 7, 8};
    CHECK("hot loop warmup", hot_loop(data, 1000000) == 4500000);
    j = sigsetjmp(env, 1);
    if (j == 0) hot_loop(NULL, 10);
    CHECK("fault in JIT-compiled loop", j == SIGSEGV);

    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
    CHECK("sigaltstack set", sigaltstack(&ss, NULL) == 0);
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_usr2;
    sa.sa_flags = SA_ONSTACK;
    sigaction(SIGUSR2, &sa, NULL);
    raise(SIGUSR2);
    CHECK("handler runs on alternate stack", on_alt == 1);

    sigset_t set, old;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    got_usr1 = 0;
    sigprocmask(SIG_BLOCK, &set, &old);
    raise(SIGUSR1);
    CHECK("blocked signal stays pending", got_usr1 == 0);
    sigprocmask(SIG_SETMASK, &old, NULL);
    CHECK("unblocking delivers it", got_usr1 == SIGUSR1);

    printf("\n%d passed, %d failed\n", passes, fails);
    return fails ? 1 : 0;
}
