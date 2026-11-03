/* aaccheck: our AAC-LC decoder (../aacdec.c) over raw frames ([u32 LE length][frame] records, ../tools/m4a2frames.py).
 *
 *   aaccheck dump FILE.frames OUT.raw [CH] [RATE]   our PCM (s16le, interleaved); frames refused, the tools used
 *   aaccheck cmp FILE.frames REF.raw [CH] [RATE] [SKIP]   sample differences against a reference decoder's s16le
 *                                         output (FDK, FFmpeg), after dropping our first SKIP frames of samples
 *                                         (the encoder's priming, when the reference honours the edit list)
 *   aaccheck fuzz FILE.frames [CH] [ROUNDS]   the frames as they are, then with bit flips, truncated, random bytes
 *   aaccheck bench FILE.frames [CH] [RATE] [ROUNDS]   CPU time against the audio's length (on the unit)
 *   aaccheck wav OUT.wav SIGNAL [CH]      a synthetic signal (10 s, 44.1 kHz) for an encoder (afconvert):
 *                                         music, transients, noise, wide (left/right differ), sweep, quiet, loud
 * CH 2 and RATE 44100 by default.
 *
 * Host:   cc -O2 -DAAC_STATS -I. -I../dsp -o /tmp/aaccheck check/aaccheck.c aacdec.c aac_tables.c ../dsp/imdct.c \
 *            ../dsp/imdct_tables.c -lm
 * Fuzz:   the same with -O1 -g -fsanitize=address,undefined -fno-sanitize-recover=undefined
 * Unit:   arm-linux-musleabi-gcc -O2 -march=armv5te -static (the same files); -DPROF with module/os/debug/prof.c
 *         profiles the bench into /tmp/aacprof.bin */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include "aacdec.h"

#ifdef AAC_STATS
extern struct {
    uint32_t frames, eight_short, start_stop, kbd, tns_filters, pulses, ms1, ms2, is_bands, pns_bands, escapes,
        cpe_common, cpe_separate;
} aac_stats;
#endif

static struct aac_dec dec;

struct frames {
    uint8_t *data;
    const uint8_t **f;
    uint32_t *len;
    int n;
};

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

static struct frames read_frames(const char *path) {
    struct frames r = {0};
    size_t len;
    r.data = slurp(path, &len);
    for (size_t i = 0; i + 4 <= len;) {
        uint32_t l = r.data[i] | r.data[i + 1] << 8 | r.data[i + 2] << 16 | (uint32_t)r.data[i + 3] << 24;
        if (l > len - i - 4) {
            fprintf(stderr, "%s: a record runs past the end\n", path);
            exit(1);
        }
        r.f = realloc(r.f, (r.n + 1) * sizeof *r.f);
        r.len = realloc(r.len, (r.n + 1) * sizeof *r.len);
        r.f[r.n] = r.data + i + 4;
        r.len[r.n++] = l;
        i += 4 + l;
    }
    return r;
}

static void init(const char *path, unsigned rate, int ch) {
    if (aac_init(&dec, rate, ch)) {
        fprintf(stderr, "%s: %u Hz, %d channels: not supported\n", path, rate, ch);
        exit(1);
    }
}

/* every frame decoded; refused ones leave no samples. *n = samples (all channels) */
static int16_t *decode(const char *path, int ch, unsigned rate, size_t *n) {
    struct frames fr = read_frames(path);
    init(path, rate, ch);
    int16_t *pcm = malloc((size_t)fr.n * AAC_FRAME * ch * 2 + 1);
    int refused = 0, first = -1, first_err = 0;
    *n = 0;
    for (int i = 0; i < fr.n; i++) {
        int k = aac_decode(&dec, fr.f[i], fr.len[i], pcm + *n);
        if (k < 0) {
            if (first < 0)
                first = i, first_err = k;
            refused++;
            continue;
        }
        *n += k;
    }
    fprintf(stderr, "%s: %d frames, %d refused", path, fr.n, refused);
    if (first >= 0)
        fprintf(stderr, " (the first: frame %d, %d)", first, first_err);
    fprintf(stderr, "\n");
#ifdef AAC_STATS
    fprintf(stderr,
            "tools: %u eight-short, %u start/stop, %u KBD, %u TNS filters, %u pulses, M/S %u per band %u all, "
            "%u intensity bands, %u noise bands, %u escapes, pairs %u common %u separate\n",
            aac_stats.eight_short, aac_stats.start_stop, aac_stats.kbd, aac_stats.tns_filters, aac_stats.pulses,
            aac_stats.ms1, aac_stats.ms2, aac_stats.is_bands, aac_stats.pns_bands, aac_stats.escapes,
            aac_stats.cpe_common, aac_stats.cpe_separate);
#endif
    return pcm;
}

static int cmp(const char *path, const char *refpath, int ch, unsigned rate, int skip) {
    size_t n, rlen;
    int16_t *ours = decode(path, ch, rate, &n), *ref = (int16_t *)slurp(refpath, &rlen);
    size_t off = (size_t)skip * AAC_FRAME * ch;
    off = off < n ? off : n;
    rlen /= 2;
    size_t m = n - off < rlen ? n - off : rlen, at = 0, over1 = 0;
    int maxd = 0;
    double se = 0, sr = 0;
    for (size_t i = 0; i < m; i++) {
        int d = abs(ours[off + i] - ref[i]);
        if (d > maxd)
            maxd = d, at = i;
        over1 += d > 1;
        se += (double)d * d;
        sr += (double)ref[i] * ref[i];
    }
    printf("%s: %zu / %zu samples (ours / ref), max diff %d LSB at %zu (frame %zu, ch %zu), rms diff %.3f LSB, "
           "%zu > 1 LSB, ref rms %.0f\n",
           path, n - off, rlen, maxd, at, at / ch, at % ch, m ? sqrt(se / m) : 0.0, over1, m ? sqrt(sr / m) : 0.0);
    return maxd > 4;
}

static uint32_t rng = 7;
static uint32_t rnd(void) {
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

static int fuzz(const char *path, int ch, int rounds) {
    struct frames fr = read_frames(path);
    static int16_t pcm[2 * AAC_FRAME];
    static uint8_t buf[4096];
    long ok = 0, err = 0;
    init(path, 44100, ch);
    for (int i = 0; i < fr.n; i++)
        *(aac_decode(&dec, fr.f[i], fr.len[i], pcm) < 0 ? &err : &ok) += 1;
    printf("clean: %ld decoded, %ld refused\n", ok, err);
    ok = err = 0;
    for (int r = 0; r < rounds; r++) {
        init(path, 44100, ch);
        for (int i = 0; i < fr.n; i++) {
            uint32_t l = fr.len[i] < sizeof buf ? fr.len[i] : sizeof buf;
            memcpy(buf, fr.f[i], l);
            switch (rnd() % 4) {
            case 0:
                for (unsigned k = 0; l && k < 1 + rnd() % 8; k++)
                    buf[rnd() % l] ^= 1 << (rnd() % 8);
                break;
            case 1:
                l = rnd() % (l + 1);
                break;
            case 2:
                for (uint32_t k = l ? rnd() % l : 0; k < l; k++)
                    buf[k] = rnd();
                break;
            default:
                l = rnd() % sizeof buf;
                for (uint32_t k = 0; k < l; k++)
                    buf[k] = rnd();
            }
            int k = aac_decode(&dec, buf, l, pcm);
            if (k > AAC_FRAME * ch) {
                printf("FAIL: %d samples\n", k);
                return 1;
            }
            *(k < 0 ? &err : &ok) += 1;
        }
    }
    printf("mutated: %ld decoded, %ld refused, no fault\n", ok, err);
    return 0;
}

static double cpu(void) {
    struct rusage r;
    getrusage(RUSAGE_SELF, &r);
    return r.ru_utime.tv_sec + r.ru_utime.tv_usec * 1e-6 + r.ru_stime.tv_sec + r.ru_stime.tv_usec * 1e-6;
}

static int bench(const char *path, int ch, unsigned rate, int rounds) {
    struct frames fr = read_frames(path);
    static int16_t pcm[2 * AAC_FRAME];
    long frames = 0, errs = 0;
    long long sum = 0;
#ifdef PROF
    void zd1_prof_start(void);
    int zd1_prof_stop(const char *);
    zd1_prof_start();
#endif
    double t0 = cpu();
    for (int r = 0; r < rounds; r++) {
        init(path, rate, ch);
        for (int i = 0; i < fr.n; i++, frames++) {
            int n = aac_decode(&dec, fr.f[i], fr.len[i], pcm);
            if (n < 0)
                errs++;
            for (int k = 0; k < n; k += 97)
                sum += pcm[k];
        }
    }
#ifdef PROF
    zd1_prof_stop("/tmp/aacprof.bin");
#endif
    double t = cpu() - t0, audio = frames * (double)AAC_FRAME / rate;
    printf("%ld frames (%ld refused): %.2f s cpu for %.1f s audio = %.1f %% of the CPU, %.0f us a frame (check %lld)\n",
           frames, errs, t, audio, 100 * t / audio, t * 1e6 / frames, sum);
    return 0;
}

static double test_signal(const char *kind, double t, int c) {
    const double pi2 = 2 * M_PI;
    double r = (int32_t)rnd() / 2147483648.0;
    if (!strcmp(kind, "music"))
        return 0.25 * sin(pi2 * 220 * t) + 0.15 * (0.5 + 0.5 * sin(pi2 * 0.5 * t)) * sin(pi2 * 440 * t + c) +
               0.1 * sin(pi2 * 1760 * t) * sin(pi2 * 3 * t) + 0.05 * sin(pi2 * 5000 * t) + 0.02 * r;
    if (!strcmp(kind, "transients")) {
        double ph = t * 4 - floor(t * 4);
        return (ph < 0.01 ? 0.9 * r : 0) + 0.3 * sin(pi2 * 330 * t) * exp(-ph * 8);
    }
    if (!strcmp(kind, "noise"))
        return 0.4 * r * (0.6 + 0.4 * sin(pi2 * 0.3 * t));
    if (!strcmp(kind, "wide"))
        return c ? 0.4 * sin(pi2 * 450 * t) * cos(pi2 * 1.1 * t) : 0.4 * sin(pi2 * 300 * t) + 0.1 * r;
    if (!strcmp(kind, "sweep"))
        return 0.5 * sin(pi2 * (50 * t + 1000 * t * t));
    if (!strcmp(kind, "quiet"))
        return 0.0005 * sin(pi2 * 1000 * t) + 0.00005 * r;
    if (!strcmp(kind, "loud")) {
        double v = 1.3 * sin(pi2 * 100 * t);
        return (v > 1 ? 1 : v < -1 ? -1 : v) * 0.999;
    }
    fprintf(stderr, "no signal %s\n", kind);
    exit(2);
}

static void le(FILE *f, uint32_t v, int n) {
    for (int i = 0; i < n; i++)
        fputc(v >> (8 * i) & 0xFF, f);
}

static int wav(const char *path, const char *kind, int ch) {
    const uint32_t rate = 44100, n = 10 * rate, data = n * ch * 2;
    FILE *f = fopen(path, "wb");
    if (!f) {
        perror(path);
        return 1;
    }
    fwrite("RIFF", 1, 4, f);
    le(f, 36 + data, 4);
    fwrite("WAVEfmt ", 1, 8, f);
    le(f, 16, 4);
    le(f, 1, 2);
    le(f, ch, 2);
    le(f, rate, 4);
    le(f, rate * 2 * ch, 4);
    le(f, 2 * ch, 2);
    le(f, 16, 2);
    fwrite("data", 1, 4, f);
    le(f, data, 4);
    for (uint32_t i = 0; i < n; i++)
        for (int c = 0; c < ch; c++) {
            double v = test_signal(kind, (double)i / rate, c);
            v = v > 1 ? 1 : v < -1 ? -1 : v;
            le(f, (uint16_t)(int16_t)(v * 32767), 2);
        }
    fclose(f);
    return 0;
}

int main(int argc, char **argv) {
    const char *cmd = argc > 2 ? argv[1] : "";
    int a3 = argc > 3 ? atoi(argv[3]) : 0, a4 = argc > 4 ? atoi(argv[4]) : 0, a5 = argc > 5 ? atoi(argv[5]) : 0;
    if ((!strcmp(cmd, "dump") || !strcmp(cmd, "cmp")) && argc > 3) {
        int ch = a4 ? a4 : 2;
        unsigned rate = a5 ? a5 : 44100;
        if (!strcmp(cmd, "cmp"))
            return cmp(argv[2], argv[3], ch, rate, argc > 6 ? atoi(argv[6]) : 0);
        size_t n;
        int16_t *pcm = decode(argv[2], ch, rate, &n);
        FILE *f = fopen(argv[3], "wb");
        if (!f || fwrite(pcm, 2, n, f) != n) {
            perror(argv[3]);
            return 1;
        }
        fclose(f);
        return 0;
    }
    if (!strcmp(cmd, "fuzz"))
        return fuzz(argv[2], a3 ? a3 : 2, a4 ? a4 : 20);
    if (!strcmp(cmd, "bench"))
        return bench(argv[2], a3 ? a3 : 2, a4 ? a4 : 44100, a5 ? a5 : 3);
    if (!strcmp(cmd, "wav") && argc > 3)
        return wav(argv[2], argv[3], argc > 4 ? atoi(argv[4]) : 2);
    fprintf(stderr, "usage: aaccheck dump FILE OUT [CH] [RATE] | cmp FILE REF [CH] [RATE] [SKIP] | "
                    "fuzz FILE [CH] [ROUNDS] | bench FILE [CH] [RATE] [ROUNDS] | wav OUT SIGNAL [CH]\n");
    return 2;
}
