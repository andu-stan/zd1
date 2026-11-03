/* vorbischeck: our Vorbis decoder (../vorbisdec.c) on a stream: an Ogg Vorbis file, or its packets (u32le length +
 * bytes, the three headers first: what `packets` writes and Warp0's hang replay holds).
 *
 *   vorbischeck dump FILE OUT.raw       our PCM (s16le, interleaved; an Ogg file's trimmed to its last granule
 *                                       position); what the stream uses
 *   vorbischeck cmp FILE.ogg REF.raw    sample differences against a reference decoder's s16le output (libvorbis
 *                                       through `oggdec -R`, Tremor)
 *   vorbischeck packets FILE.ogg OUT    the stream's packets, for module/warp0/tools/emu.py
 *   vorbischeck fuzz FILE [ROUNDS]      the packets as they are, then with bit flips, truncation, random bytes and
 *                                       shuffled order, restarts; the setup header mutated too. Nothing may fault,
 *                                       and every result is a sample count within VORBIS_MAX_OUT or an error
 *   vorbischeck bench FILE [ROUNDS]     CPU time against the audio's length (on the unit)
 *
 * Host:   cc -O2 -DVORBIS_STATS -I. -I../dsp -o /tmp/vorbischeck check/vorbischeck.c vorbisdec.c vorbis_tables.c \
 *            ../dsp/imdct.c ../dsp/imdct_tables.c -lm
 * Fuzz:   the same with -O1 -g -fsanitize=address,undefined -fno-sanitize-recover=undefined
 * Unit:   arm-linux-musleabi-gcc -O2 -march=armv5te -static (the same files) */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include "vorbisdec.h"

#ifdef VORBIS_STATS
extern struct {
    uint32_t packets, short_blocks, long_blocks, transitions, floor_unused, eop, books_type1, books_type2,
        residue_type[3], coupled, saturated;
} vorbis_stats;
#endif

static uint8_t *slurp(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        perror(path);
        exit(1);
    }
    fseek(f, 0, SEEK_END);
    *len = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *d = malloc(*len + 1);
    if (fread(d, 1, *len, f) != *len) {
        perror(path);
        exit(1);
    }
    fclose(f);
    return d;
}

/* ---- a stream's packets, from an Ogg file (one logical stream, packets in order) or a packets file ---- */

struct stream {
    uint8_t **p;
    uint32_t *len;
    int n;
    int64_t last_granule; /* -1: not known (a packets file) */
};

static void add(struct stream *s, const uint8_t *p, uint32_t len) {
    s->p = realloc(s->p, (s->n + 1) * sizeof *s->p);
    s->len = realloc(s->len, (s->n + 1) * sizeof *s->len);
    s->p[s->n] = malloc(len + 1);
    memcpy(s->p[s->n], p, len);
    s->len[s->n++] = len;
}

static void read_ogg(struct stream *s, const uint8_t *d, size_t len) {
    static uint8_t pkt[1 << 20];
    size_t pos = 0, plen = 0;
    while (pos + 27 <= len) {
        if (memcmp(d + pos, "OggS", 4)) {
            pos++;
            continue;
        }
        const uint8_t *h = d + pos;
        int nseg = h[26];
        if (pos + 27 + nseg > len)
            break;
        int64_t g = 0;
        for (int i = 7; i >= 0; i--)
            g = g << 8 | h[6 + i];
        if (g != -1)
            s->last_granule = g;
        size_t body = pos + 27 + nseg;
        for (int i = 0; i < nseg; i++) {
            int n = h[27 + i];
            if (body + n > len || plen + n > sizeof pkt)
                return;
            memcpy(pkt + plen, d + body, n);
            body += n;
            plen += n;
            if (n < 255) {
                add(s, pkt, (uint32_t)plen);
                plen = 0;
            }
        }
        pos = body;
    }
}

static struct stream load(const char *path) {
    struct stream s = {0, 0, 0, -1};
    size_t len;
    uint8_t *d = slurp(path, &len);
    if (len >= 4 && !memcmp(d, "OggS", 4)) {
        read_ogg(&s, d, len);
    } else {
        for (size_t i = 0; i + 4 <= len;) {
            uint32_t l = d[i] | d[i + 1] << 8 | d[i + 2] << 16 | (uint32_t)d[i + 3] << 24;
            if (l > len - i - 4)
                break;
            add(&s, d + i + 4, l);
            i += 4 + l;
        }
    }
    free(d);
    if (s.n < 3) {
        fprintf(stderr, "%s: no headers\n", path);
        exit(1);
    }
    return s;
}

static struct vorbis_dec dec;
static uint8_t arena[2 << 20], fast[16 << 10];
static int16_t pcm[VORBIS_MAX_OUT * VORBIS_MAX_CH + 64];

static void init(const char *path, const struct stream *s) {
    int r = vorbis_init(&dec, s->p[0], s->len[0], s->p[2], s->len[2], arena, sizeof arena, arena, sizeof arena);
    if (r) {
        fprintf(stderr, "%s: vorbis_init %d\n", path, r);
        exit(1);
    }
}

/* the whole stream decoded; *n = samples (all channels) */
static int16_t *decode(const char *path, int *channels, size_t *n) {
    struct stream s = load(path);
    init(path, &s);
    unsigned fu, su;
    vorbis_arena_used(&dec, &fu, &su);
    fprintf(stderr, "%s: %u Hz, %d ch, %d kb/s nominal, blocks %u/%u, %u modes, rp %d, setup %u bytes\n", path,
            dec.info.rate, dec.info.channels, dec.info.bitrate_nominal / 1000, dec.info.blocksize[0],
            dec.info.blocksize[1], dec.info.modes, dec.rp, fu);
    int ch = dec.info.channels, errs = 0;
    size_t cap = 1 << 20, cnt = 0;
    int16_t *out = malloc(cap * 2);
    for (int i = 3; i < s.n; i++) {
        int k = vorbis_decode(&dec, s.p[i], s.len[i], pcm);
        if (k < 0) {
            errs++;
            continue;
        }
        if (cnt + (size_t)k * ch > cap) {
            cap = (cnt + k * ch) * 2;
            out = realloc(out, cap * 2);
        }
        memcpy(out + cnt, pcm, (size_t)k * ch * 2);
        cnt += (size_t)k * ch;
    }
    if (s.last_granule >= 0 && (size_t)s.last_granule * ch < cnt)
        cnt = (size_t)s.last_granule * ch;
    if (errs)
        fprintf(stderr, "%s: %d packets refused\n", path, errs);
#ifdef VORBIS_STATS
    fprintf(stderr,
            "stats: %u packets (%u short, %u long, %u transitions), floors unused %u, ended early %u, "
            "residue types %u/%u/%u, coupled %u\n",
            vorbis_stats.packets, vorbis_stats.short_blocks, vorbis_stats.long_blocks, vorbis_stats.transitions,
            vorbis_stats.floor_unused, vorbis_stats.eop, vorbis_stats.residue_type[0], vorbis_stats.residue_type[1],
            vorbis_stats.residue_type[2], vorbis_stats.coupled);
#endif
    *channels = ch;
    *n = cnt;
    return out;
}

static int cmp(const char *path, const char *refpath) {
    int ch;
    size_t n, rlen;
    int16_t *ours = decode(path, &ch, &n), *ref = (int16_t *)slurp(refpath, &rlen);
    rlen /= 2;
    size_t m = n < rlen ? n : rlen, at = 0, over1 = 0;
    int maxd = 0;
    double se = 0, sr = 0;
    for (size_t i = 0; i < m; i++) {
        int d = abs(ours[i] - ref[i]);
        if (d > maxd)
            maxd = d, at = i;
        over1 += d > 1;
        se += (double)d * d;
        sr += (double)ref[i] * ref[i];
    }
    printf("%s: %zu / %zu samples (ours / ref), max diff %d LSB at %zu (frame %zu, ch %zu), rms diff %.3f LSB, "
           "%zu > 1 LSB, ref rms %.0f\n",
           path, n, rlen, maxd, at, at / ch, at % ch, m ? sqrt(se / m) : 0.0, over1, m ? sqrt(sr / m) : 0.0);
    return n != rlen || maxd > 4;
}

static uint32_t rng = 0x9E3779B9;
static uint32_t rnd(void) {
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

static int fuzz(const char *path, int rounds) {
    struct stream s = load(path);
    static uint8_t buf[65536], setup[65536];
    int inits = 0, refused = 0, decoded = 0, errors = 0;
    for (int r = 0; r < rounds; r++) {
        /* the setup: as is (two arenas, so the fast/slow split is exercised), or mutated */
        uint32_t sl = s.len[2] < sizeof setup ? s.len[2] : sizeof setup;
        memcpy(setup, s.p[2], sl);
        if (r % 4 == 3) {
            int flips = rnd() % 2 ? 1 : 1 + rnd() % 8;
            for (int k = 0; sl && k < flips; k++)
                setup[rnd() % sl] ^= (uint8_t)(1 << (rnd() % 8));
        }
        inits++;
        if (vorbis_init(&dec, s.p[0], s.len[0], setup, sl, fast, sizeof fast, arena, sizeof arena)) {
            refused++;
            continue;
        }
        for (int i = 3; i < s.n; i++) {
            int j = r % 4 == 2 ? 3 + (int)(rnd() % (s.n - 3)) : i; /* shuffled order */
            uint32_t len = s.len[j] < sizeof buf ? s.len[j] : sizeof buf;
            memcpy(buf, s.p[j], len);
            switch (rnd() % 8) {
            case 0:
                buf[rnd() % (len ? len : 1)] ^= (uint8_t)(1 << (rnd() % 8));
                break;
            case 1:
                len = len ? rnd() % len : 0;
                break;
            case 2:
                for (uint32_t k = 0; k < len; k++)
                    buf[k] = (uint8_t)rnd();
                break;
            case 3:
                if (len)
                    buf[0] &= 0xFE; /* keep it an audio packet */
                break;
            default:
                break;
            }
            int k = vorbis_decode(&dec, buf, len, pcm);
            if (k > VORBIS_MAX_OUT) {
                printf("FAIL: %d samples\n", k);
                return 1;
            }
            *(k < 0 ? &errors : &decoded) += 1;
            if (rnd() % 64 == 0)
                vorbis_restart(&dec);
        }
    }
    printf("fuzz: %d inits (%d refused), %d packets decoded, %d refused: ok\n", inits, refused, decoded, errors);
    return 0;
}

static double cpu_s(void) {
    struct rusage u;
    getrusage(RUSAGE_SELF, &u);
    return u.ru_utime.tv_sec + u.ru_stime.tv_sec + (u.ru_utime.tv_usec + u.ru_stime.tv_usec) / 1e6;
}

static double wall_s(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int bench(const char *path, int rounds) {
    struct stream s = load(path);
    double c0 = cpu_s(), w0 = wall_s(), ci = 0;
    long samples = 0;
    for (int r = 0; r < rounds; r++) {
        double t = cpu_s();
        init(path, &s);
        ci += cpu_s() - t;
        for (int i = 3; i < s.n; i++) {
            int k = vorbis_decode(&dec, s.p[i], s.len[i], pcm);
            if (k > 0)
                samples += k;
        }
    }
    double c = cpu_s() - c0, w = wall_s() - w0, audio = (double)samples / dec.info.rate;
    unsigned fu, su;
    vorbis_arena_used(&dec, &fu, &su);
    printf("%s: %u Hz %u ch: %.1f s of audio in %.2f s cpu (%.2f wall): %.1f %% of the CPU at real time; "
           "setup %.1f ms, %u bytes\n",
           path, dec.info.rate, dec.info.channels, audio, c, w, 100 * c / audio, 1000 * ci / rounds, fu);
    return 0;
}

int main(int argc, char **argv) {
    const char *cmd = argc > 2 ? argv[1] : "";
    if (!strcmp(cmd, "dump") && argc > 3) {
        int ch;
        size_t n;
        int16_t *out = decode(argv[2], &ch, &n);
        FILE *f = fopen(argv[3], "wb");
        if (!f || fwrite(out, 2, n, f) != n) {
            perror(argv[3]);
            return 1;
        }
        fclose(f);
        return 0;
    }
    if (!strcmp(cmd, "cmp") && argc > 3)
        return cmp(argv[2], argv[3]);
    if (!strcmp(cmd, "packets") && argc > 3) {
        struct stream s = load(argv[2]);
        FILE *f = fopen(argv[3], "wb");
        if (!f) {
            perror(argv[3]);
            return 1;
        }
        for (int i = 0; i < s.n; i++) {
            uint8_t l[4] = {s.len[i], s.len[i] >> 8, s.len[i] >> 16, s.len[i] >> 24};
            fwrite(l, 1, 4, f);
            fwrite(s.p[i], 1, s.len[i], f);
        }
        fclose(f);
        return 0;
    }
    if (!strcmp(cmd, "fuzz"))
        return fuzz(argv[2], argc > 3 ? atoi(argv[3]) : 20);
    if (!strcmp(cmd, "bench"))
        return bench(argv[2], argc > 3 ? atoi(argv[3]) : 3);
    fprintf(stderr, "usage: vorbischeck dump FILE OUT | cmp FILE.ogg REF | packets FILE.ogg OUT | fuzz FILE [ROUNDS] | "
                    "bench FILE [ROUNDS]\n");
    return 2;
}
