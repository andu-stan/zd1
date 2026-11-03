/* Fixed-point IMDCT for N = 2048 and 256 (imdct.c), shared by the AAC and Vorbis decoders: both use the standard
 * MDCT, x[n] = sum_k X[k] cos(2 pi / N (n + N/4 + 1/2)(k + 1/2)).
 *
 * An N/4-point complex FFT between pre- and post-twiddles, in block floating point: each block is normalised to
 * below 2^27, the FFT scales by 1/N (radix-4 stages: Q30 twiddles and a high-word multiply are the /4), each twiddled
 * component is one rounded 64-bit multiply-accumulate. Output in "PCM Q9" for inputs in AAC's spectral units (Q2 of
 * 16-bit PCM): out = 2^9 * (2 / N) * sum_k (X[k] / 4) cos(...). */
#ifndef DSP_IMDCT_H
#define DSP_IMDCT_H
#include <stdint.h>

/* DSP_FAST: placement of the tables (every block reads all of them); empty by default, a section attribute where
 * they go to fast RAM (Warp0's local RAM) */
#ifndef DSP_FAST
#define DSP_FAST
#endif

extern const int32_t dsp_imdct_tw_2048[1024], dsp_imdct_tw_256[128]; /* cos, sin of -2pi(k+1/8)/N, Q31 */
extern const int32_t dsp_fft_tw[768];                                /* cos, sin of -2pi j/512, j < 384, Q30 */
extern const uint16_t dsp_bitrev9[512];

/* The IMDCT of x (n/2 coefficients, n = 2048 or 256) up to the post-twiddled values: z = n/4 complex values (n/2
 * int32, the caller's scratch), PCM Q9. Returns 0 (z untouched) if the block is silent. */
int imdct_z(int32_t *z, const int32_t *x, int n);

/* The whole IMDCT: out = n time samples, PCM Q9 (z: n/2 int32 of scratch) */
void imdct(int32_t *z, const int32_t *x, int n, int32_t *out);

/* A 2048-point block's time samples in pairs, straight from imdct_z's z: y[p] and y[1024 + p] come from the same
 * z value, so the IMDCT's reorder, both window halves and the overlap-add can be one pass (AAC's long blocks,
 * Vorbis's long block after a long one). Runs the statements given for every p < 1024 with a = y[p] and
 * b = y[1024 + p], all int32_t (PCM Q9). */
#define DSP_IMDCT_LONG_PAIRS(z, ...)                                                                                   \
    for (int k_ = 0; k_ < 256; k_++) {                                                                                 \
        int p;                                                                                                         \
        int32_t a, b;                                                                                                  \
        p = 2 * k_, a = (z)[2 * (256 + k_)], b = (z)[2 * (256 + k_) + 1];                                              \
        __VA_ARGS__;                                                                                                   \
        p = 2 * k_ + 1, a = -(z)[2 * (255 - k_) + 1], b = -(z)[2 * (255 - k_)];                                        \
        __VA_ARGS__;                                                                                                   \
        p = 512 + 2 * k_, a = (z)[2 * k_ + 1], b = -(z)[2 * k_];                                                       \
        __VA_ARGS__;                                                                                                   \
        p = 512 + 2 * k_ + 1, a = -(z)[2 * (511 - k_)], b = (z)[2 * (511 - k_) + 1];                                   \
        __VA_ARGS__;                                                                                                   \
    }

#endif
