/* Fixed-point helpers the decoders share (module/codecs, Warp0's ALAC). No libc: the same code builds for Warp0
 * (MIPS I: no divider, no clz, an unaligned word access faults). */
#ifndef DSP_FIXED_H
#define DSP_FIXED_H
#include <stdint.h>

typedef int64_t i64;
typedef uint64_t u64;
typedef uint32_t __attribute__((may_alias)) dsp_word_t;

/* bytes: a multiple of 4, p word aligned (an unaligned word store traps on the ARM and faults on Warp0) */
static inline void dsp_zero(void *p, unsigned bytes) {
    dsp_word_t *w = p;
    for (unsigned i = 0; i < bytes / 4; i++)
        w[i] = 0;
}

static inline int32_t sat32(i64 v) { return v > 0x7FFFFFFF ? 0x7FFFFFFF : v < -0x7FFFFFFF ? -0x7FFFFFFF : (int32_t)v; }
static inline int32_t mulhi(int32_t a, int32_t b) { return (int32_t)(((i64)a * b) >> 32); }
/* (a * b + c * d) >> 32, rounded once: one complex multiply component (SMULL + SMLAL on the ARM926) */
static inline int32_t mac2(int32_t a, int32_t b, int32_t c, int32_t d) {
    return (int32_t)(((i64)a * b + (i64)c * d + 0x80000000LL) >> 32);
}

static inline int clz32(uint32_t x) {
#if defined(__GNUC__) && !defined(__mips__)
    return x ? __builtin_clz(x) : 32;
#else
    int n = 0;
    if (!x)
        return 32;
    if (!(x & 0xFFFF0000)) {
        n += 16;
        x <<= 16;
    }
    if (!(x & 0xFF000000)) {
        n += 8;
        x <<= 8;
    }
    if (!(x & 0xF0000000)) {
        n += 4;
        x <<= 4;
    }
    if (!(x & 0xC0000000)) {
        n += 2;
        x <<= 2;
    }
    if (!(x & 0x80000000))
        n += 1;
    return n;
#endif
}

/* n / d by shift and subtract (no divide instruction on Warp0); d != 0 */
static inline u64 div64_32(u64 n, uint32_t d) {
    u64 q = 0, r = 0;
    for (int i = 63; i >= 0; i--) {
        r = (r << 1) | ((n >> i) & 1);
        if (r >= d) {
            r -= d;
            q |= (u64)1 << i;
        }
    }
    return q;
}

/* n / d, 32 bits; d != 0 */
static inline uint32_t udiv32(uint32_t n, uint32_t d) {
    uint32_t q = 0, r = 0;
    for (int i = 31; i >= 0; i--) {
        r = (r << 1) | ((n >> i) & 1);
        if (r >= d) {
            r -= d;
            q |= 1u << i;
        }
    }
    return q;
}

#endif
