/* Debug aid for C ports on the module OS: link this object in and a SIGSEGV/SIGBUS prints the faulting pc, lr and
 * address (map them with arm-linux-musleabi-addr2line -f -e <unstripped binary>), then exits 139. The 2.6.24 kernel
 * has no CONFIG_DEBUG_USER, so it doesn't say where. */
#include <signal.h>
#include <stdio.h>
#include <ucontext.h>
#include <unistd.h>

static void report(int sig, siginfo_t *si, void *ctx) {
    ucontext_t *uc = ctx;
    char b[160];
    int n = snprintf(b, sizeof b, "zd1-segv: signal %d at pc %08lx lr %08lx sp %08lx addr %p\n", sig,
                     uc->uc_mcontext.arm_pc, uc->uc_mcontext.arm_lr, uc->uc_mcontext.arm_sp, si->si_addr);
    write(2, b, n);
    _exit(139);
}

__attribute__((constructor)) static void install(void) {
    struct sigaction sa = {0};
    static char alt[16384];
    stack_t ss = {.ss_sp = alt, .ss_size = sizeof alt};
    sigaltstack(&ss, 0); /* a stack overflow still gets reported */
    sa.sa_sigaction = report;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGBUS, &sa, 0);
}
