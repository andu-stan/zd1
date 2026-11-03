/* flaccheck: our FLAC decoder (../flacdec.c) on .flac files, and what a libFLAC corpus (corpus.py) can't show.
 *
 *   flaccheck dump FILE.flac OUT.raw      every frame decoded, the samples in the stream's own width, little-endian
 *                                         (what `flac -d --force-raw-format --endian=little --sign=signed` writes)
 *   flaccheck chunks FILE.flac OUT.chunks [N]   STREAMINFO, then N frames per chunk (default 1), each as u32 length +
 *                                         bytes: what a Snapcast server's chunks hold (module/warp0/tools/emu.py flac)
 *   flaccheck escape                      frames built bit by bit with escaped residual partitions (raw residuals of
 *                                         a given width, 0 included, next to Rice partitions): libFLAC's encoder
 *                                         never writes them
 *   flaccheck fuzz N FILE.flac...         N mutated copies of each file's frames (bit flips, truncation, random
 *                                         bytes, random output room), the input allocated to exactly what the
 *                                         decoder may read
 *   flaccheck bench FILE.flac [ROUNDS]    CPU time against the audio's length (on the unit)
 *
 * Host:   cc -O2 -Wall -Wextra -I. -I../dsp -o /tmp/flaccheck check/flaccheck.c flacdec.c (-DFLAC_STATS: the tools a stream
 *         exercises; the ARM's limits: -DFLAC_MAX_CH=8 -DFLAC_MAX_BLOCK=65535)
 * Fuzz:   the same with -g -O1 -fsanitize=address,undefined -fno-sanitize-recover
 * Unit:   arm-linux-musleabi-gcc -O2 -march=armv5te -static (the same files) */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include "flacdec.h"

static uint8_t *slurp(const char *path, long *len) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        perror(path);
        exit(1);
    }
    fseek(f, 0, SEEK_END);
    *len = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *b = malloc(*len + 8);
    if (fread(b, 1, *len, f) != (size_t)*len) {
        perror(path);
        exit(1);
    }
    fclose(f);
    memset(b + *len, 0, 8);
    return b;
}

/* "fLaC", the metadata blocks; the STREAMINFO body and where the frames start */
static long frames_start(const uint8_t *b, long len, const uint8_t **si) {
    long at = 4;
    if (len < 8 || memcmp(b, "fLaC", 4)) {
        fprintf(stderr, "not a FLAC file\n");
        exit(1);
    }
    *si = 0;
    for (;;) {
        if (at + 4 > len) {
            fprintf(stderr, "metadata runs past the file\n");
            exit(1);
        }
        int last = b[at] >> 7, type = b[at] & 0x7F;
        long n = (long)b[at + 1] << 16 | b[at + 2] << 8 | b[at + 3];
        if (type == 0)
            *si = b + at + 4;
        at += 4 + n;
        if (last)
            break;
    }
    if (!*si) {
        fprintf(stderr, "no STREAMINFO\n");
        exit(1);
    }
    return at;
}

/* the frames from `at` copied to a word-aligned buffer (the decoder's contract) */
static uint32_t *aligned(const uint8_t *src, long n) {
    uint32_t *w = calloc((n + 7) / 4 + 2, 4);
    memcpy(w, src, n);
    return w;
}

/* ---- a bit writer and the frame CRCs ---- */
static uint8_t wbuf[4096];
static unsigned wpos; /* bits */

static void put(uint32_t v, unsigned n) {
    while (n--) {
        if (v >> n & 1)
            wbuf[wpos >> 3] |= 0x80 >> (wpos & 7);
        wpos++;
    }
}

static unsigned crc8(const uint8_t *p, unsigned n) {
    unsigned c = 0;
    while (n--) {
        c ^= *p++;
        for (int k = 0; k < 8; k++)
            c = c & 0x80 ? (c << 1 ^ 7) & 0xFF : c << 1 & 0xFF;
    }
    return c;
}

static unsigned crc16(const uint8_t *p, unsigned n) {
    unsigned c = 0;
    while (n--) {
        c ^= (unsigned)*p++ << 8;
        for (int k = 0; k < 8; k++)
            c = c & 0x8000 ? (c << 1 ^ 0x8005) & 0xFFFF : c << 1 & 0xFFFF;
    }
    return c;
}

static const uint8_t streaminfo_mono16[34] = {
    0, 16, 0, 64, 0, 0, 0, 0, 0, 0, 0x0A, 0xC4, 0x40, 0xF0}; /* 44100 Hz, 1 ch, 16 bits, blocks 16-64 */

/* a mono 16-bit frame of `n` samples: FIXED order `order`, warm-up `warm`, partition order `porder`, each partition's
 * residuals either Rice with k = 2 (esc[p] < 0) or escaped at width esc[p]; the residuals are res[] */
static unsigned build(unsigned n, unsigned order, const int32_t *warm, unsigned porder, const int *esc,
                      const int32_t *res) {
    memset(wbuf, 0, sizeof wbuf);
    wpos = 0;
    put(0xFFF8, 16); /* sync, reserved, fixed blocking */
    put(6, 4);
    put(9, 4);
    put(0, 4);
    put(4, 3);
    put(0, 1); /* block: 8-bit size, 44.1 kHz, mono, 16 bits */
    put(0, 8); /* frame number 0 */
    put(n - 1, 8);
    put(crc8(wbuf, wpos >> 3), 8);
    put(0, 1);
    put(8 + order, 6);
    put(0, 1); /* FIXED, no wasted bits */
    for (unsigned i = 0; i < order; i++)
        put((uint32_t)warm[i] & 0xFFFF, 16);
    put(0, 2);
    put(porder, 4); /* Rice, partition order */
    unsigned i = order;
    for (unsigned p = 0; p < 1u << porder; p++) {
        unsigned end = (p + 1) * (n >> porder);
        if (esc[p] >= 0) {
            put(15, 4);
            put((unsigned)esc[p], 5);
            for (; i < end; i++)
                if (esc[p])
                    put((uint32_t)res[i - order] & ((1u << esc[p]) - 1), (unsigned)esc[p]);
        } else {
            put(2, 4);
            for (; i < end; i++) {
                uint32_t u = res[i - order] < 0 ? (uint32_t)(-2 * res[i - order] - 1) : (uint32_t)(2 * res[i - order]);
                put(1, (u >> 2) + 1); /* q zeros, then the one */
                put(u & 3, 2);
            }
        }
    }
    if (wpos & 7)
        put(0, 8 - (wpos & 7));
    unsigned bytes = wpos >> 3, c = crc16(wbuf, bytes);
    put(c, 16);
    return wpos >> 3;
}

static int check_escape(unsigned n, unsigned order, unsigned porder, const int *esc) {
    static struct flac_dec d;
    int32_t warm[4] = {1000, -2000, 300, 7}, res[64], want[64], got[64];
    srand(n * 31 + order * 7 + porder);
    for (unsigned i = 0; i < order; i++)
        want[i] = warm[i];
    for (unsigned p = 0, i = order; p < 1u << porder; p++) {
        for (unsigned end = (p + 1) * (n >> porder); i < end; i++) {
            int32_t *x = want, pred = order == 0   ? 0
                                      : order == 1 ? x[i - 1]
                                      : order == 2 ? 2 * x[i - 1] - x[i - 2]
                                      : order == 3 ? 3 * (x[i - 1] - x[i - 2]) + x[i - 3]
                                                   : 4 * (x[i - 1] + x[i - 3]) - 6 * x[i - 2] - x[i - 4];
            if (esc[p] == 0)
                res[i - order] = 0; /* a 0-bit escape: zeros */
            else if (esc[p] > 6) {  /* a wide escape: a bounded signal, its residual whatever it is */
                int32_t t = rand() % 6001 - 3000;
                res[i - order] = t - pred;
            } else
                res[i - order] = rand() % 31 - 15; /* fits 5 bits signed, and Rice with k = 2 */
            want[i] = pred + res[i - order];
        }
    }
    // cppcheck-suppress uninitvar ; the loop above sets res[0..n - order)
    unsigned len = build(n, order, warm, porder, esc, res);
    uint32_t *in = calloc((len + 3) / 4, 4), used, samples;
    memcpy(in, wbuf, len);
    if (flac_init(&d, streaminfo_mono16, 34)) {
        puts("init");
        return 1;
    }
    int r = flac_decode(&d, (const uint8_t *)in, len, got, 64, &used, &samples);
    free(in);
    int bad = r != 0 || used != len || samples != n;
    for (unsigned i = 0; !bad && i < n; i++)
        bad = got[i] != (int32_t)((uint32_t)want[i] << 16);
    printf("%s: %u samples, fixed order %u, partition order %u, escapes", bad ? "FAIL" : "ok", n, order, porder);
    for (unsigned p = 0; p < 1u << porder; p++)
        printf(" %d", esc[p]);
    printf(" (result %d, %u of %u bytes, %u samples)\n", r, used, len, samples);
    return bad;
}

static int escape(void) {
    static const int all6[1] = {6}, zero[1] = {0}, mixed[4] = {6, -1, 0, 5}, rice[4] = {-1, -1, -1, -1},
                     mixed8[8] = {-1, 5, 0, -1, 6, 6, -1, 0}, wide[4] = {18, 18, 18, 18};
    int bad = 0;
    bad += check_escape(16, 0, 0, all6);
    bad += check_escape(16, 0, 0, zero);
    bad += check_escape(32, 2, 2, mixed);
    bad += check_escape(32, 2, 2, rice);
    bad += check_escape(64, 1, 3, mixed8);
    bad += check_escape(64, 4, 2, wide);
    printf("%s\n", bad ? "escape: FAILED" : "escape: all decoded as built");
    return bad != 0;
}

/* ---- fuzz ---- */
static int fuzz(int n, int argc, char **argv) {
    static struct flac_dec d;
    static int32_t out[4 * FLAC_MAX_BLOCK * FLAC_MAX_CH];
    unsigned long runs = 0, errors = 0, ok = 0;
    srand(12345);
    for (int a = 0; a < argc; a++) {
        long len;
        uint8_t *f = slurp(argv[a], &len);
        const uint8_t *si;
        long at = frames_start(f, len, &si);
        long total = len - at;
        if (total > 200000)
            total = 200000; /* the start of each file is enough */
        for (int k = 0; k < n; k++) {
            if (flac_init(&d, si, 34)) {
                printf("%s: init\n", argv[a]);
                return 1;
            }
            long n2 = total;
            int how = rand() % 4;
            if (how == 1)
                n2 = rand() % (total + 1); /* truncated */
            uint32_t *in = malloc(((n2 + 3) / 4) * 4 + (n2 == 0));
            memcpy(in, f + at, n2);
            memset((uint8_t *)in + n2, 0, ((n2 + 3) / 4) * 4 - n2);
            uint8_t *p = (uint8_t *)in;
            if (how == 0 || how == 3) /* bit flips */
                for (int j = 1 + rand() % 8; j && n2; j--)
                    p[rand() % n2] ^= 1 << (rand() % 8);
            if (how == 2 && n2 > 16) { /* a run of random bytes */
                long s = rand() % (n2 - 16), m = 1 + rand() % 16;
                for (long j = 0; j < m; j++)
                    p[s + j] = rand();
            }
            uint32_t pos = 0;
            while (pos < n2) {
                uint32_t cap = 1 + rand() % (sizeof out / sizeof out[0]), used, samples;
                /* each call reads from a word boundary: copy the rest down (as a caller would) */
                uint32_t rest = n2 - pos, *w = malloc(((rest + 3) / 4) * 4);
                memcpy(w, p + pos, rest);
                memset((uint8_t *)w + rest, 0, ((rest + 3) / 4) * 4 - rest);
                int r = flac_decode(&d, (const uint8_t *)w, rest, out, cap, &used, &samples);
                free(w);
                runs++;
                if (samples > cap || used > rest) {
                    printf("FAIL: out of bounds result\n");
                    return 1;
                }
                if (r) {
                    errors++;
                    break;
                }
                ok++;
                if (!used)
                    break;
                pos += used;
            }
            free(in);
        }
        free(f);
    }
    printf("fuzz: %lu decode calls, %lu ended in an error, %lu clean, no fault\n", runs, errors, ok);
    return 0;
}

static struct flac_dec dec;

/* the file's STREAMINFO into the decoder; its frames word-aligned in *data, *total bytes */
static void open_file(const char *path, uint32_t **data, uint32_t *total) {
    long len;
    uint8_t *file = slurp(path, &len);
    const uint8_t *si;
    long at = frames_start(file, len, &si);
    int r = flac_init(&dec, si, 34);
    if (r) {
        fprintf(stderr, "%s: flac_init %d\n", path, r);
        exit(1);
    }
    *data = aligned(file + at, len - at);
    *total = (uint32_t)(len - at);
    free(file);
}

static int dump(const char *path, FILE *out) {
    uint32_t *data, total, pos = 0, ch;
    open_file(path, &data, &total);
    ch = dec.info.channels;
    /* odd output sizes on purpose: the decoder must stop before a frame that doesn't fit and go on from there */
    static int32_t pcm[3 * FLAC_MAX_BLOCK * FLAC_MAX_CH];
    uint32_t caps[3] = {FLAC_MAX_BLOCK * FLAC_MAX_CH, 3 * FLAC_MAX_BLOCK * FLAC_MAX_CH, (dec.info.max_block + 7) * ch},
             k = 0;
    uint32_t bytes = (dec.info.bits + 7) / 8;
    long samples = 0;
    while (pos < total) {
        uint32_t used, n;
        /* the decoder reads the input as aligned words: give it the rest from a word boundary */
        uint32_t *w = aligned((const uint8_t *)data + pos, total - pos);
        int r = flac_decode(&dec, (const uint8_t *)w, total - pos, pcm, caps[k++ % 3], &used, &n);
        free(w);
        for (uint32_t i = 0; i < n; i++) {
            int32_t v = pcm[i] >> (32 - 8 * bytes);
            fwrite(&v, 1, bytes, out); /* little-endian host: the low bytes first */
        }
        samples += n;
        if (r) {
            fprintf(stderr, "frame at byte %u: error %d\n", pos + used, r);
            return 1;
        }
        if (!used) {
            fprintf(stderr, "no progress at byte %u\n", pos);
            return 1;
        }
        pos += used;
    }
    fprintf(stderr, "%u frames, %ld samples per channel, %u Hz %u ch %u bits, blocks %u-%u\n", dec.frames,
            samples / (long)ch, dec.info.rate, ch, dec.info.bits, dec.info.min_block, dec.info.max_block);
    return 0;
}

static int chunks(const char *path, FILE *out, uint32_t per) {
    uint32_t *data, total, pos = 0, ch, nchunks = 0;
    open_file(path, &data, &total);
    ch = dec.info.channels;
    static int32_t pcm[FLAC_MAX_BLOCK * FLAC_MAX_CH];
    long len;
    uint8_t *file = slurp(path, &len);
    const uint8_t *si;
    frames_start(file, len, &si);
    uint32_t n34 = 34;
    fwrite(&n34, 4, 1, out);
    fwrite(si, 1, 34, out);
    free(file);
    while (pos < total) {
        uint32_t start = pos;
        for (uint32_t f = 0; f < per && pos < total; f++) {
            uint32_t used, n;
            uint32_t *w = aligned((const uint8_t *)data + pos, total - pos);
            /* room for exactly one frame: the decoder takes one and says where the next starts */
            int r = flac_decode(&dec, (const uint8_t *)w, total - pos, pcm, dec.info.max_block * ch, &used, &n);
            free(w);
            if (r || !used) {
                fprintf(stderr, "frame at byte %u: error %d\n", pos, r);
                return 1;
            }
            pos += used;
        }
        uint32_t n = pos - start;
        fwrite(&n, 4, 1, out);
        fwrite((const uint8_t *)data + start, 1, n, out);
        nchunks++;
    }
    fprintf(stderr, "%u chunks of up to %u frames\n", nchunks, per);
    return 0;
}

static double cpu(void) {
    struct rusage r;
    getrusage(RUSAGE_SELF, &r);
    return r.ru_utime.tv_sec + r.ru_utime.tv_usec * 1e-6 + r.ru_stime.tv_sec + r.ru_stime.tv_usec * 1e-6;
}

/* the frames decoded in place, as zd1-snapcast hands them over */
static int bench(const char *path, int rounds) {
    uint32_t *data, total;
    open_file(path, &data, &total);
    static int32_t pcm[FLAC_MAX_BLOCK * FLAC_MAX_CH];
    long samples = 0;
    double t0 = cpu();
    for (int k = 0; k < rounds; k++) {
        for (uint32_t pos = 0; pos < total;) {
            uint32_t used, n;
            int r =
                flac_decode_from(&dec, (const uint8_t *)data, total, pos, pcm, sizeof pcm / sizeof pcm[0], &used, &n);
            if (r || used <= pos) {
                fprintf(stderr, "frame at byte %u: error %d\n", pos, r);
                return 1;
            }
            samples += n;
            pos = used;
        }
    }
    double t = cpu() - t0, audio = (double)samples / dec.info.channels / dec.info.rate;
    printf("%s: %u Hz %u ch %u bits: %.1f s of audio in %.2f s cpu: %.1f %% of the CPU at real time\n", path,
           dec.info.rate, dec.info.channels, dec.info.bits, audio, t, 100 * t / audio);
    return 0;
}

int main(int argc, char **argv) {
    const char *cmd = argc > 1 ? argv[1] : "";
    if (!strcmp(cmd, "escape"))
        return escape();
    if (!strcmp(cmd, "fuzz") && argc > 3)
        return fuzz(atoi(argv[2]), argc - 3, argv + 3);
    if (!strcmp(cmd, "bench") && argc > 2)
        return bench(argv[2], argc > 3 ? atoi(argv[3]) : 3);
    if ((!strcmp(cmd, "dump") || !strcmp(cmd, "chunks")) && argc > 3) {
        FILE *out = fopen(argv[3], "wb");
        if (!out) {
            perror(argv[3]);
            return 1;
        }
        int r =
            !strcmp(cmd, "dump") ? dump(argv[2], out) : chunks(argv[2], out, argc > 4 ? (uint32_t)atoi(argv[4]) : 1);
        fclose(out);
#ifdef FLAC_STATS
        static const char *names[FLAC_NSTATS] = {"constant",  "verbatim",   "fixed",    "lpc",
                                                 "lpc64",     "wasted",     "escape",   "rice2",
                                                 "left/side", "right/side", "mid/side", "independent"};
        for (int i = 0; i < FLAC_NSTATS; i++)
            fprintf(stderr, "%s%s %lu", i ? ", " : "stats: ", names[i], flac_stats[i]);
        fprintf(stderr, "\n");
#endif
        return r;
    }
    fprintf(stderr, "usage: flaccheck dump FILE.flac OUT.raw | chunks FILE.flac OUT.chunks [N] | escape | "
                    "fuzz N FILE.flac... | bench FILE.flac [ROUNDS]\n");
    return 2;
}
