/* wifi IFACE                                  the Wireless-Extensions view of a link: AP, channel, bit rate, TX power,
 *                                             power management, retry limit, RTS / fragmentation thresholds, quality
 * wifi IFACE rate BPS|auto | retry N | rts N|off   the setters, for link experiments */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <net/if.h>
#include <linux/wireless.h>
static int s;
static const char *ifn;
static void init(struct iwreq *w) {
    memset(w, 0, sizeof *w);
    strncpy(w->ifr_name, ifn, IFNAMSIZ - 1);
}
static int get(int req, struct iwreq *w) {
    init(w);
    return ioctl(s, req, w);
}
static void param(const char *name, int req, const char *unit) {
    struct iwreq w;
    if (get(req, &w) < 0) {
        printf("%-10s ?\n", name);
        return;
    }
    printf("%-10s %d%s fixed=%d disabled=%d flags=0x%x\n", name, w.u.param.value, unit, w.u.param.fixed,
           w.u.param.disabled, w.u.param.flags);
}
static int show(void) {
    struct iwreq w;
    if (get(SIOCGIWAP, &w) == 0) {
        unsigned char *m = (unsigned char *)w.u.ap_addr.sa_data;
        printf("ap         %02x:%02x:%02x:%02x:%02x:%02x\n", m[0], m[1], m[2], m[3], m[4], m[5]);
    }
    if (get(SIOCGIWFREQ, &w) == 0) {
        double f = w.u.freq.m;
        for (int i = 0; i < w.u.freq.e; i++)
            f *= 10;
        printf("freq       %g\n", f);
    }
    param("rate", SIOCGIWRATE, " b/s");
    param("txpower", SIOCGIWTXPOW, " dBm");
    param("power", SIOCGIWPOWER, "");
    param("retry", SIOCGIWRETRY, "");
    param("rts", SIOCGIWRTS, "");
    param("frag", SIOCGIWFRAG, "");
    param("sens", SIOCGIWSENS, "");
    struct iw_statistics st;
    init(&w);
    w.u.data.pointer = &st;
    w.u.data.length = sizeof st;
    w.u.data.flags = 1;
    if (ioctl(s, SIOCGIWSTATS, &w) == 0)
        printf("qual       %d level %d noise %d updated 0x%x; discard nwid %u crypt %u frag %u retries %u misc %u; "
               "missed beacons %u\n",
               st.qual.qual, (signed char)st.qual.level, (signed char)st.qual.noise, st.qual.updated, st.discard.nwid,
               st.discard.code, st.discard.fragment, st.discard.retries, st.discard.misc, st.miss.beacon);
    return 0;
}
static int set(const char *what, const char *v) {
    struct iwreq w;
    init(&w);
    int req, off = !strcmp(v, "off"), aut = !strcmp(v, "auto");
    if (!strcmp(what, "rate")) {
        req = SIOCSIWRATE;
        w.u.bitrate.value = aut ? -1 : atoi(v);
        w.u.bitrate.fixed = !aut;
    } else if (!strcmp(what, "retry")) {
        req = SIOCSIWRETRY;
        w.u.retry.value = atoi(v);
        w.u.retry.flags = IW_RETRY_LIMIT;
    } else if (!strcmp(what, "rts")) {
        req = SIOCSIWRTS;
        w.u.rts.value = off ? 2347 : atoi(v);
        w.u.rts.disabled = off;
        w.u.rts.fixed = 1;
    } else {
        fprintf(stderr, "wifi: %s? (rate, retry, rts)\n", what);
        return 2;
    }
    if (ioctl(s, req, &w) < 0) {
        perror(what);
        return 1;
    }
    return 0;
}
int main(int argc, char **argv) {
    if (argc != 2 && argc != 4) {
        fprintf(stderr, "usage: wifi IFACE | wifi IFACE rate BPS|auto | retry N | rts N|off\n");
        return 2;
    }
    ifn = argv[1];
    s = socket(AF_INET, SOCK_DGRAM, 0);
    return argc == 2 ? show() : set(argv[2], argv[3]);
}
