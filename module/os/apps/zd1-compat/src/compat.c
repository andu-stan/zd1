/* System calls Linux 2.6.24 lacks, for std and tokio programs on the module (the linker routes calls here with
 * --wrap; lib.rs). Each shim tries the real call first, so a newer kernel takes the normal path.
 *
 * - futex FUTEX_WAIT_BITSET (2.6.25): Rust's std waits on every futex with it (Mutex under contention, Condvar,
 *   thread::park, channels), through musl's syscall(). On ENOSYS std returns at once and every blocking wait
 *   becomes a busy loop. Here: FUTEX_WAIT with the absolute timeout made relative.
 * - epoll_create1 and eventfd2 (2.6.27): tokio's I/O driver (mio) passes CLOEXEC / NONBLOCK, and musl falls back to
 *   the old calls only for flags 0, so the runtime didn't start. Here: the old call, then fcntl.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <stdarg.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

long __real_syscall(long n, ...);
int __real_epoll_create1(int flags);
int __real_eventfd(unsigned int count, int flags);

int zd1_compat_linked(void) { return 1; }

static int futex_wait_bitset_missing;

/* The futex system call here is the time32 one (SYS_futex = 240 on ARM), and Rust's libc crate uses a 32-bit
 * time_t on 32-bit musl, so the timespecs going in and out are two 32-bit words, not musl 1.2's struct timespec. */
struct ts32 {
    int32_t sec, nsec;
};

static long futex_wait_compat(long uaddr, long op, long val, const struct ts32 *abs) {
    struct ts32 rel;
    struct timespec now;
    if (abs) {
        clock_gettime((op & FUTEX_CLOCK_REALTIME) ? CLOCK_REALTIME : CLOCK_MONOTONIC, &now);
        int64_t ns = ((int64_t)abs->sec - now.tv_sec) * 1000000000 + (abs->nsec - now.tv_nsec);
        if (ns <= 0) {
            errno = ETIMEDOUT;
            return -1;
        }
        rel.sec = (int32_t)(ns / 1000000000);
        rel.nsec = (int32_t)(ns % 1000000000);
    }
    return __real_syscall(SYS_futex, uaddr, FUTEX_WAIT | (op & FUTEX_PRIVATE_FLAG), val, abs ? &rel : 0, 0, 0);
}

long __wrap_syscall(long n, ...) {
    va_list ap;
    va_start(ap, n);
    long a = va_arg(ap, long), b = va_arg(ap, long), c = va_arg(ap, long);
    long d = va_arg(ap, long), e = va_arg(ap, long), f = va_arg(ap, long);
    va_end(ap);
    if (n == SYS_futex && (b & FUTEX_CMD_MASK) == FUTEX_WAIT_BITSET && (unsigned)f == FUTEX_BITSET_MATCH_ANY) {
        if (futex_wait_bitset_missing)
            return futex_wait_compat(a, b, c, (const struct ts32 *)d);
        long r = __real_syscall(n, a, b, c, d, e, f);
        if (r == -1 && errno == ENOSYS) {
            futex_wait_bitset_missing = 1;
            return futex_wait_compat(a, b, c, (const struct ts32 *)d);
        }
        return r;
    }
    return __real_syscall(n, a, b, c, d, e, f);
}

int __wrap_epoll_create1(int flags) {
    int fd = __real_epoll_create1(flags);
    if (fd >= 0 || errno != ENOSYS)
        return fd;
    fd = __real_epoll_create1(0); /* musl: SYS_epoll_create for flags 0 */
    if (fd >= 0 && (flags & EPOLL_CLOEXEC))
        fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

int __wrap_eventfd(unsigned int count, int flags) {
    int fd = __real_eventfd(count, flags);
    if (fd >= 0 || errno != ENOSYS)
        return fd;
    fd = __real_eventfd(count, 0); /* musl: SYS_eventfd for flags 0 */
    if (fd >= 0) {
        if (flags & EFD_NONBLOCK)
            fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK);
        if (flags & EFD_CLOEXEC)
            fcntl(fd, F_SETFD, FD_CLOEXEC);
    }
    return fd;
}
