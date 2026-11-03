/* soak: measures the CPU left over, for loads the kernel's tick-sampled accounting misses (Linux 2.6.24, HZ=100: a
 * thread that runs between two ticks is never charged). Runs as SCHED_IDLE, so it only gets cycles nobody else
 * wants, and prints how many loop iterations it got per second. Calibrate on an idle system (`soak 5`), then the
 * load of anything else is 1 - rate / idle rate.  usage: soak [SECONDS]  (prints one line per second) */
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <sys/syscall.h>
#include <unistd.h>

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec * 1e-9;
}

int main(int argc, char **argv) {
    int secs = argc > 1 ? atoi(argv[1]) : 30;
    struct sched_param p = {0};
    /* musl's sched_setscheduler() is a stub (ENOSYS): the system call itself */
    if (syscall(SYS_sched_setscheduler, 0, 5 /* SCHED_IDLE */, &p))
        perror("soak: SCHED_IDLE (running at normal priority)");
    volatile unsigned long x = 0;
    for (int s = 0; s < secs; s++) {
        double t0 = now(), t;
        unsigned long n = 0;
        do {
            for (int i = 0; i < 1000; i++)
                x += i;
            n++;
        } while ((t = now()) - t0 < 1.0);
        printf("%lu\n", (unsigned long)(n / (t - t0)));
        fflush(stdout);
    }
    return 0;
}
