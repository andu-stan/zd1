/* A sampling profiler for the module, where there is no perf: SIGPROF at the kernel's tick (HZ = 100) while the
 * process runs, each sample the interrupted user PC and LR (kernel time lands on the system call's return address).
 * zd1_prof_stop writes the samples as pairs of little-endian u32 for symbolising against the unstripped binary
 * (profsym.py). Linked into a C program to profile it: the codecs' benches (module/codecs/aac/check/aaccheck.c bench, -DPROF).
 */
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/time.h>
#include <ucontext.h>

#define PROF_MAX 65536
static uint32_t prof[PROF_MAX][2];
static volatile int prof_n;

static void on_prof(int sig, siginfo_t *si, void *ctx) {
    (void)sig;
    (void)si;
    ucontext_t *uc = ctx;
    int i = __sync_fetch_and_add(&prof_n, 1);
    if (i < PROF_MAX) {
#ifdef __arm__
        prof[i][0] = uc->uc_mcontext.arm_pc;
        prof[i][1] = uc->uc_mcontext.arm_lr;
#else /* a host build (test runs on 64-bit Linux): no samples worth symbolising */
        (void)uc;
        prof[i][0] = prof[i][1] = 0;
#endif
    }
}

void zd1_prof_start(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = on_prof;
    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    sigaction(SIGPROF, &sa, 0);
    struct itimerval t = {{0, 1000}, {0, 1000}};
    setitimer(ITIMER_PROF, &t, 0);
}

int zd1_prof_stop(const char *path) {
    struct itimerval z = {{0, 0}, {0, 0}};
    setitimer(ITIMER_PROF, &z, 0);
    int n = prof_n < PROF_MAX ? prof_n : PROF_MAX;
    FILE *f = fopen(path, "wb");
    if (!f)
        return -1;
    fwrite(prof, 8, n, f);
    fclose(f);
    return n;
}
