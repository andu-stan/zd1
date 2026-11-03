/* udpprobe: a paced stream of numbered UDP packets and what arrived of it, to characterise a network path the way an
 * AirPlay realtime stream uses it (one ~1.2 KB packet every 8 ms). The Mac side is tools/zeppelin/lab/udpprobe.py
 * (the same packets and summary).
 *
 *   udpprobe recv PORT SECONDS          print the summary of what arrived
 *   udpprobe send HOST PORT INTERVAL_US SIZE SECONDS
 *
 * Packet: u32 sequence, u64 send time (µs, sender's clock), padding. Summary: received / expected, lost, duplicates,
 * out of order, loss bursts by length, arrival gaps (largest, how many over 20/50/100 ms), and a per-second line of
 * losses. Static, musl: `nix build .#debug-tools` (bin/udpprobe, with the other probes here). */
#include <arpa/inet.h>
#include <netdb.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000 + t.tv_nsec / 1000;
}

static int do_send(const char *host, int port, int interval, int size, int seconds) {
    struct addrinfo hints = {.ai_family = AF_INET, .ai_socktype = SOCK_DGRAM}, *ai;
    char ps[16];
    snprintf(ps, sizeof ps, "%d", port);
    if (getaddrinfo(host, ps, &hints, &ai)) {
        fprintf(stderr, "udpprobe: %s?\n", host);
        return 1;
    }
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    unsigned char buf[2048] = {0};
    if (size < 12)
        size = 12;
    if (size > (int)sizeof buf)
        size = sizeof buf;
    uint64_t t0 = now_us();
    uint32_t n = (uint32_t)((uint64_t)seconds * 1000000 / interval);
    for (uint32_t i = 0; i < n; i++) {
        uint64_t due = t0 + (uint64_t)i * interval, t;
        while ((t = now_us()) < due)
            if (due - t > 2000)
                usleep(due - t - 1000);
        uint32_t seq = htonl(i);
        memcpy(buf, &seq, 4);
        memcpy(buf + 4, &t, 8);
        sendto(s, buf, size, 0, ai->ai_addr, ai->ai_addrlen);
    }
    printf("sent %u packets of %d B every %d us\n", n, size, interval);
    return 0;
}

#define MAXP 200000
static uint8_t seen[MAXP];

static int do_recv(int port, int seconds) {
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    int rcv = 256 * 1024;
    setsockopt(s, SOL_SOCKET, SO_RCVBUF, &rcv, sizeof rcv);
    struct sockaddr_in a = {.sin_family = AF_INET, .sin_port = htons(port)};
    if (bind(s, (struct sockaddr *)&a, sizeof a)) {
        perror("bind");
        return 1;
    }
    struct timeval tv = {.tv_sec = 1};
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    unsigned char buf[2048];
    uint64_t start = 0, last = 0, end = now_us() + (uint64_t)seconds * 1000000 + 30000000;
    uint32_t got = 0, dup = 0, ooo = 0, hi = 0, first = 0;
    uint32_t gap20 = 0, gap50 = 0, gap100 = 0;
    uint64_t maxgap = 0;
    int any = 0;
    while (now_us() < end) {
        ssize_t r = recv(s, buf, sizeof buf, 0);
        if (r < 12) {
            if (any && now_us() - last > 2000000)
                break; /* the sender stopped */
            continue;
        }
        uint64_t t = now_us();
        uint32_t seq;
        memcpy(&seq, buf, 4);
        seq = ntohl(seq);
        if (!any) {
            any = 1;
            start = t;
            first = seq;
            end = t + (uint64_t)seconds * 1000000 + 3000000;
        } else {
            uint64_t g = t - last;
            if (g > maxgap)
                maxgap = g;
            gap20 += g > 20000;
            gap50 += g > 50000;
            gap100 += g > 100000;
        }
        last = t;
        if (seq >= MAXP)
            continue;
        if (seen[seq]) {
            dup++;
            continue;
        }
        seen[seq] = 1;
        got++;
        if (seq < hi)
            ooo++;
        else
            hi = seq;
    }
    if (!any) {
        printf("nothing received\n");
        return 1;
    }
    uint32_t expected = hi - first + 1, lost = 0, bursts[5] = {0}, run = 0;
    for (uint32_t i = first; i <= hi + 1; i++) {
        if (i <= hi && !seen[i]) {
            lost++;
            run++;
        } else if (run) {
            bursts[run == 1 ? 0 : run == 2 ? 1 : run <= 5 ? 2 : run <= 10 ? 3 : 4]++;
            run = 0;
        }
    }
    printf("received %u of %u (%.2f %% lost), %u duplicates, %u out of order, over %.1f s\n", got, expected,
           100.0 * lost / expected, dup, ooo, (last - start) / 1e6);
    printf("loss bursts: 1:%u 2:%u 3-5:%u 6-10:%u >10:%u\n", bursts[0], bursts[1], bursts[2], bursts[3], bursts[4]);
    printf("arrival gaps: max %.1f ms, >20 ms %u, >50 ms %u, >100 ms %u\n", maxgap / 1000.0, gap20, gap50, gap100);
    /* losses per second (packets 125 a second at 8 ms): one character each, . = none, 1-9, * = 10 or more */
    uint32_t per = expected * 1000000ULL / (last - start + 1) + 1;
    printf("lost per second: ");
    for (uint32_t i = first; i <= hi; i += per) {
        uint32_t l = 0;
        for (uint32_t j = i; j < i + per && j <= hi; j++)
            l += !seen[j];
        putchar(l == 0 ? '.' : l < 10 ? '0' + l : '*');
    }
    putchar('\n');
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 4 && !strcmp(argv[1], "recv"))
        return do_recv(atoi(argv[2]), atoi(argv[3]));
    if (argc == 7 && !strcmp(argv[1], "send"))
        return do_send(argv[2], atoi(argv[3]), atoi(argv[4]), atoi(argv[5]), atoi(argv[6]));
    fprintf(stderr, "usage: udpprobe recv PORT SECONDS | send HOST PORT INTERVAL_US SIZE SECONDS\n");
    return 2;
}
