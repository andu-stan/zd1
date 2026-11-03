/* Host test of alac.c against golden vectors (Apple's encoder: the PCM is the oracle): shairplay-rust's 16-bit ones
 * and ../vectors (stereo352 / mono352 16-bit, stereo24 / mono24 24-bit from vectors/afalac24.py).
 *   cc -DALAC_MAX_FRAME=4096 -fsanitize=address,undefined -I../../codecs/dsp -o /tmp/alac_test alac_test.c alac.c
 *   /tmp/alac_test .../tests/data/alac/stereo.alac .../mono.alac ../vectors/NAME.alac ... */
#include "alac.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint32_t le32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }
static uint32_t be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3]; }

static int check(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) {
        perror(path);
        return 1;
    }
    static uint8_t d[1 << 20];
    size_t len = fread(d, 1, sizeof d, f);
    fclose(f);
    if (len < 8 || memcmp(d, "ALACGV01", 8)) {
        printf("%s: bad magic\n", path);
        return 1;
    }
    size_t p = 8;
    p += 4; /* sample rate */
    unsigned channels = d[p], depth = d[p + 1];
    p += 4;
    uint32_t clen = le32(d + p);
    p += 4;
    const uint8_t *ck = d + p;
    p += clen;
    struct alac_config c = {be32(ck + 24), ck[29], ck[30], ck[31], ck[32], ck[33]};
    uint32_t nframes = le32(d + p);
    p += 4;
    static unsigned out[1 << 20];
    static int32_t pcm[1 << 20]; /* every sample, sign-extended */
    size_t got = 0;
    for (uint32_t i = 0; i < nframes; i++) {
        uint32_t fl = le32(d + p);
        p += 4;
        int n = alac_decode(&c, d + p, fl, out);
        if (n <= 0) {
            printf("%s: frame %u -> %d\n", path, i, n);
            return 1;
        }
        for (int k = 0; k < n; k++)
            pcm[got + k] = depth == 24 ? (int32_t)out[k] : (int16_t)(out[k / 2] >> (k & 1 ? 16 : 0));
        got += n;
        p += fl;
    }
    uint32_t plen = le32(d + p);
    p += 4;
    unsigned w = depth / 8;
    int ok = got * w == plen;
    for (size_t k = 0; ok && k < got; k++) {
        const uint8_t *s = d + p + k * w;
        int32_t want =
            w == 3 ? (int32_t)((uint32_t)(s[0] | s[1] << 8 | s[2] << 16) << 8) >> 8 : (int16_t)(s[0] | s[1] << 8);
        if (pcm[k] != want) {
            printf("%s: sample %zu: %d, want %d\n", path, k, pcm[k], want);
            ok = 0;
        }
    }
    printf("%s: %u-bit, %u channels, frame %u, %u frames, %zu samples: %s\n", path, depth, channels, c.frame_length,
           nframes, got, ok ? "matches" : "MISMATCH");
    return !ok;
}

int main(int argc, char **argv) {
    int bad = 0;
    for (int i = 1; i < argc; i++)
        bad |= check(argv[i]);
    /* robustness: random and truncated frames must not crash (ASan/UBSan watch) */
    static unsigned out[4096 * 2];
    static unsigned char buf[2600];
    srand(1);
    for (int depth = 16; depth <= 24; depth += 8) {
        struct alac_config c = {352, (unsigned)depth, 40, 10, 14, 2};
        for (int i = 0; i < 200000; i++) {
            unsigned n = rand() % sizeof buf;
            for (unsigned k = 0; k < n; k++)
                buf[k] = rand();
            if (i & 1)
                buf[0] = (buf[0] & 0x1F) | 0x20; /* a stereo element */
            if (i & 2)
                buf[2] = (buf[2] & ~0x0E) | 0x02; /* compressed, a byte shifted off */
            alac_decode(&c, buf, n, out);
        }
        printf("200000 random %d-bit frames decoded without a fault\n", depth);
    }
    return bad;
}
