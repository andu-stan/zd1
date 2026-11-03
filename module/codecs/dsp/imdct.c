/* Fixed-point IMDCT (imdct.h). Integer only, no division: builds for the ARM926 and for Warp0. */
#include "imdct.h"
#include "fixed.h"

/* Complex FFT on bit-reversed input, scaled by 1/n: radix-4 stages (blocks at j, j+L, j+2L, j+3L hold the sub-DFTs of
 * residues 0, 2, 1, 3), after one radix-2 stage when log2(n) is odd. Twiddles are Q30, so a high-word multiply
 * by one is the product / 4, the stage's scaling; components grow at most sqrt 2 per stage. */
static void fft(int32_t *z, int n) {
    int L = 1;
    if (n == 512) {
        for (int i = 0; i < 2 * n; i += 4) {
            int32_t ar = (z[i] + 1) >> 1, ai = (z[i + 1] + 1) >> 1, br = (z[i + 2] + 1) >> 1, bi = (z[i + 3] + 1) >> 1;
            z[i] = ar + br;
            z[i + 1] = ai + bi;
            z[i + 2] = ar - br;
            z[i + 3] = ai - bi;
        }
        L = 2;
    }
    for (; L < n; L <<= 2) {
        int step = 512 / (4 * L);
        for (int g = 0; g < n; g += 4 * L) {
            int32_t *p0 = z + 2 * g, *p1 = p0 + 2 * L, *p2 = p1 + 2 * L, *p3 = p2 + 2 * L;
            const int32_t *w1 = dsp_fft_tw, *w2 = dsp_fft_tw, *w3 = dsp_fft_tw;
            for (int j = 0; j < L;
                 j++, p0 += 2, p1 += 2, p2 += 2, p3 += 2, w1 += 2 * step, w2 += 4 * step, w3 += 6 * step) {
                int32_t t0r = (p0[0] + 2) >> 2, t0i = (p0[1] + 2) >> 2;
                int32_t t1r = mac2(p2[0], w1[0], p2[1], -w1[1]), t1i = mac2(p2[0], w1[1], p2[1], w1[0]);
                int32_t t2r = mac2(p1[0], w2[0], p1[1], -w2[1]), t2i = mac2(p1[0], w2[1], p1[1], w2[0]);
                int32_t t3r = mac2(p3[0], w3[0], p3[1], -w3[1]), t3i = mac2(p3[0], w3[1], p3[1], w3[0]);
                int32_t s02r = t0r + t2r, s02i = t0i + t2i, d02r = t0r - t2r, d02i = t0i - t2i;
                int32_t s13r = t1r + t3r, s13i = t1i + t3i, d13r = t1r - t3r, d13i = t1i - t3i;
                p0[0] = s02r + s13r;
                p0[1] = s02i + s13i;
                p2[0] = s02r - s13r;
                p2[1] = s02i - s13i;
                p1[0] = d02r + d13i;
                p1[1] = d02i - d13r;
                p3[0] = d02r - d13i;
                p3[1] = d02i + d13r;
            }
        }
    }
}

int imdct_z(int32_t *z, const int32_t *x, int n) {
    int n2 = n / 2, n4 = n / 4, lg4 = n == 2048 ? 9 : 6;
    const int32_t *tw = n == 2048 ? dsp_imdct_tw_2048 : dsp_imdct_tw_256;
    uint32_t m = 0;
    for (int i = 0; i < n2; i++)
        // cppcheck-suppress shiftTooManyBitsSigned ; an arithmetic right shift (the sign), not a left one
        m |= (uint32_t)(x[i] ^ (x[i] >> 31));
    if (!m)
        return 0;
    int s = 27 - (32 - clz32(m)); /* inputs below 2^27: with the FFT's growth, below 2^31 */
    for (int k = 0; k < n4; k++) {
        int32_t re = x[2 * k], im = x[n2 - 1 - 2 * k];
        if (s >= 0) {
            re = (int32_t)((uint32_t)re << s);
            im = (int32_t)((uint32_t)im << s);
        } else {
            re >>= -s;
            im >>= -s;
        }
        int32_t c = tw[2 * k], sn = tw[2 * k + 1];
        int r = dsp_bitrev9[k << (9 - lg4)];
        z[2 * r] = mac2(re, c, im, -sn); /* Q31 twiddle: / 2 */
        z[2 * r + 1] = mac2(re, sn, im, c);
    }
    fft(z, n4); /* / n4 */
    /* z = cand * 2^(s - lg4) after the post-twiddle (cand: the sum in the input's units); PCM Q9 = cand * 2^(10 - lgn),
     * and lgn - lg4 = 2: shift right by s - 8 */
    int sh = s - 8;
    for (int k = 0; k < n4; k++) {
        int32_t re = z[2 * k], im = z[2 * k + 1], c = tw[2 * k], sn = tw[2 * k + 1];
        int32_t yr = mac2(re, c, im, -sn), yi = mac2(re, sn, im, c); /* / 2 */
        if (sh > 0) {                                                /* |y| < 2^30.5: no overflow adding half */
            yr = (yr + (1 << (sh - 1))) >> sh;
            yi = (yi + (1 << (sh - 1))) >> sh;
        } else if (sh < 0) {
            yr = sat32((i64)yr * ((i64)1 << -sh));
            yi = sat32((i64)yi * ((i64)1 << -sh));
        }
        z[2 * k] = yr;
        z[2 * k + 1] = yi;
    }
    return 1;
}

void imdct(int32_t *z, const int32_t *x, int n, int32_t *out) {
    int n4 = n / 4, n8 = n / 8;
    if (!imdct_z(z, x, n)) {
        dsp_zero(out, n * sizeof *out);
        return;
    }
    for (int k = 0; k < n8; k++) {
        out[2 * k] = z[2 * (n8 + k)];
        out[2 * k + 1] = -z[2 * (n8 - 1 - k) + 1];
        out[n4 + 2 * k] = z[2 * k + 1];
        out[n4 + 2 * k + 1] = -z[2 * (n4 - 1 - k)];
        out[2 * n4 + 2 * k] = z[2 * (n8 + k) + 1];
        out[2 * n4 + 2 * k + 1] = -z[2 * (n8 - 1 - k)];
        out[3 * n4 + 2 * k] = -z[2 * k];
        out[3 * n4 + 2 * k + 1] = z[2 * (n4 - 1 - k) + 1];
    }
}
