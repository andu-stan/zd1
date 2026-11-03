/* AAC-LC decoder, fixed point (aacdec.h). Written from ISO/IEC 14496-3 (subpart 4, the GA syntax and tools) for
 * the subset AirPlay uses; the codebooks and band tables are the standard's (aac_tables.c, generated).
 *
 * Numbers: dequantized spectra are int32 Q2 (sign * |q|^(4/3) * 2^((sf - 100) / 4) * 4, saturated at 2^30: eight
 * times full scale);
 * the IMDCT runs in block floating point (inputs normalised per block so the FFT can't overflow) and returns time
 * samples in Q8 of 16-bit PCM; windows and twiddles are Q31. No float, and no division outside noise
 * substitution (one per band, through div64_32), so the same code builds for Warp0 (MIPS I, no divider). */
#include "aacdec.h"
#include <stddef.h>
#include "aac_tables.h"
#include "fixed.h"
#include "imdct.h"

#ifdef AAC_STATS /* tool usage, for tests (check/) */
struct {
    uint32_t frames, eight_short, start_stop, kbd, tns_filters, pulses, ms1, ms2, is_bands, pns_bands, escapes,
        cpe_common, cpe_separate;
} aac_stats;
#define STAT(f, n) (aac_stats.f += (n))
#else
#define STAT(f, n) ((void)0)
#endif

enum { ZERO_HCB = 0, ESC_HCB = 11, NOISE_HCB = 13, INTENSITY_HCB2 = 14, INTENSITY_HCB = 15 };
enum { ONLY_LONG = 0, LONG_START = 1, EIGHT_SHORT = 2, LONG_STOP = 3 };
enum { ID_SCE = 0, ID_CPE = 1, ID_CCE = 2, ID_LFE = 3, ID_DSE = 4, ID_PCE = 5, ID_FIL = 6, ID_END = 7 };

/* ---- small helpers (no libc: the same file builds for Warp0) ---- */

/* the window info a channel pair's second channel shares (struct aac_ics up to and including swb), in words: the
 * compiler would merge byte copies into unaligned word moves, which Warp0's MIPS I build must not use */
#define ICS_INFO_WORDS ((offsetof(struct aac_ics, swb) + sizeof(const uint16_t *)) / 4)

static inline int32_t clip30(i64 v) { return v > (1 << 30) ? (1 << 30) : v < -(1 << 30) ? -(1 << 30) : (int32_t)v; }

static uint32_t isqrt64(u64 x) {
    u64 r = 0, bit = (u64)1 << 62;
    while (bit > x)
        bit >>= 2;
    while (bit) {
        if (x >= r + bit) {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else
            r >>= 1;
        bit >>= 2;
    }
    return (uint32_t)r;
}

/* ---- bit reader (zeros past the end; `pos` keeps counting, so overruns are detected) ---- */

struct br {
    const uint8_t *p;
    unsigned len, byte, pos;
    uint32_t cache;
    int bits;
};

static void br_init(struct br *b, const uint8_t *p, unsigned len) {
    b->p = p;
    b->len = len;
    b->byte = 0;
    b->pos = 0;
    b->cache = 0;
    b->bits = 0;
}

static inline void br_fill(struct br *b) {
    while (b->bits <= 24) {
        uint32_t v = b->byte < b->len ? b->p[b->byte] : 0;
        b->byte++;
        b->cache |= v << (24 - b->bits);
        b->bits += 8;
    }
}

static inline uint32_t br_peek(struct br *b, int n) {
    br_fill(b);
    return b->cache >> (32 - n);
}
static inline void br_skip(struct br *b, int n) {
    b->cache <<= n;
    b->bits -= n;
    b->pos += n;
}

static inline uint32_t br_get(struct br *b, int n) {
    if (!n)
        return 0;
    uint32_t v = br_peek(b, n);
    br_skip(b, n);
    return v;
}

static inline int br_over(const struct br *b) { return b->pos > b->len * 8; }

static inline uint32_t huff(struct br *b, const uint32_t *t) {
    uint32_t e = t[br_peek(b, 8)];
    if (e & 0x8000) {
        br_skip(b, 8);
        e = t[((e >> 16) & 0x7FFF) + br_peek(b, e & 0x1F)];
    }
    br_skip(b, e & 0x1F);
    return e >> 16;
}

/* ---- side info ---- */

static int ics_info(struct aac_dec *d, struct br *b, struct aac_ics *s) {
    if (br_get(b, 1))
        return AAC_EDATA; /* ics_reserved_bit */
    s->win_seq = br_get(b, 2);
    s->win_shape = br_get(b, 1);
    STAT(eight_short, s->win_seq == EIGHT_SHORT);
    STAT(start_stop, s->win_seq == LONG_START || s->win_seq == LONG_STOP);
    STAT(kbd, s->win_shape);
    s->num_groups = 1;
    s->group_len[0] = 1;
    if (s->win_seq == EIGHT_SHORT) {
        s->max_sfb = br_get(b, 4);
        uint32_t grouping = br_get(b, 7);
        s->num_windows = 8;
        for (int i = 0; i < 7; i++) {
            if (grouping & (1u << (6 - i)))
                s->group_len[s->num_groups - 1]++;
            else
                s->group_len[s->num_groups++] = 1;
        }
        s->swb = aac_swb_short[d->sf_index];
        s->num_swb = aac_num_swb_short[d->sf_index];
        s->tns_max = aac_tns_max_bands_short[d->sf_index];
    } else {
        s->max_sfb = br_get(b, 6);
        s->num_windows = 1;
        if (br_get(b, 1))
            return AAC_EUNSUP; /* predictor_data_present: Main / LTP profiles */
        s->swb = aac_swb_long[d->sf_index];
        s->num_swb = aac_num_swb_long[d->sf_index];
        s->tns_max = aac_tns_max_bands_long[d->sf_index];
    }
    return s->max_sfb > s->num_swb ? AAC_EDATA : 0;
}

static int section_data(struct br *b, struct aac_ics *s) {
    int bits = s->win_seq == EIGHT_SHORT ? 3 : 5, esc = (1 << bits) - 1;
    for (int g = 0; g < s->num_groups; g++) {
        int k = 0;
        while (k < s->max_sfb) {
            int cb = br_get(b, 4), len = 0, inc;
            if (cb == 12)
                return AAC_EDATA;
            while ((inc = br_get(b, bits)) == esc) {
                len += esc;
                if (len > 64)
                    return AAC_EDATA;
            }
            len += inc;
            if (k + len > s->max_sfb)
                return AAC_EDATA;
            while (len--)
                s->cb[g][k++] = cb;
            if (br_over(b))
                return AAC_EBITS;
        }
    }
    return 0;
}

static int scalefactors(struct br *b, struct aac_ics *s, int global_gain) {
    int sf = global_gain, noise = global_gain - 90, is = 0, noise_first = 1;
    for (int g = 0; g < s->num_groups; g++) {
        for (int k = 0; k < s->max_sfb; k++) {
            switch (s->cb[g][k]) {
            case ZERO_HCB:
                s->sf[g][k] = 0;
                break;
            case INTENSITY_HCB:
            case INTENSITY_HCB2:
                is += (int)huff(b, aac_huff_sf) - 60;
                s->sf[g][k] = is < -155 ? -155 : is > 100 ? 100 : is;
                break;
            case NOISE_HCB:
                if (noise_first) {
                    noise_first = 0;
                    noise += (int)br_get(b, 9) - 256;
                } else
                    noise += (int)huff(b, aac_huff_sf) - 60;
                s->sf[g][k] = noise < -100 ? -100 : noise > 155 ? 155 : noise;
                break;
            default:
                sf += (int)huff(b, aac_huff_sf) - 60;
                if (sf < 0 || sf > 255)
                    return AAC_EDATA;
                s->sf[g][k] = sf;
            }
        }
    }
    return br_over(b) ? AAC_EBITS : 0;
}

static int pulse_data(struct br *b, struct aac_ics *s) {
    s->pulse_n = br_get(b, 2) + 1;
    s->pulse_start = br_get(b, 6);
    for (int i = 0; i < s->pulse_n; i++) {
        s->pulse_off[i] = br_get(b, 5);
        s->pulse_amp[i] = br_get(b, 4);
    }
    return s->win_seq == EIGHT_SHORT || s->pulse_start >= s->num_swb ? AAC_EDATA : 0;
}

static int tns_data(struct br *b, struct aac_ics *s) {
    int is8 = s->win_seq == EIGHT_SHORT;
    for (int w = 0; w < s->num_windows; w++) {
        int n = br_get(b, is8 ? 1 : 2), res = 0;
        s->tns_w[w].n_filt = n;
        if (n)
            res = br_get(b, 1);
        for (int f = 0; f < n; f++) {
            s->tns_w[w].f[f].length = br_get(b, is8 ? 4 : 6);
            int order = br_get(b, is8 ? 3 : 5);
            if (order > (is8 ? 7 : 12))
                return AAC_EDATA;
            s->tns_w[w].f[f].order = order;
            if (order) {
                STAT(tns_filters, 1);
                s->tns_w[w].f[f].dir = br_get(b, 1);
                int compress = br_get(b, 1);
                const int32_t *map = aac_tns_coef[2 * compress + res];
                for (int i = 0; i < order; i++)
                    s->tns_w[w].f[f].coef[i] = map[br_get(b, res + 3 - compress)];
            }
        }
    }
    return 0;
}

/* ---- spectral data ---- */

static int escape(struct br *b) {
    int n = 4;
    while (br_get(b, 1))
        if (++n > 12)
            return -1;
    return (1 << n) + (int)br_get(b, n);
}

static int spectral_data(struct br *b, struct aac_ics *s, int16_t *q) {
    dsp_zero(q, AAC_FRAME * sizeof *q);
    int win = 0;
    for (int g = 0; g < s->num_groups; g++) {
        for (int k = 0; k < s->max_sfb; k++) {
            int cb = s->cb[g][k];
            if (cb == ZERO_HCB || cb >= NOISE_HCB)
                continue;
            // cppcheck-suppress arrayIndexOutOfBoundsCond ; book 12 is refused in section_data
            const uint32_t *t = aac_huff_spec[cb];
            int start = s->swb[k], width = s->swb[k + 1] - start;
            for (int w = 0; w < s->group_len[g]; w++) {
                int16_t *o = q + (win + w) * 128 + start;
                if (cb <= 4) {
                    for (int i = 0; i < width; i += 4) {
                        uint32_t v = huff(b, t);
                        for (int j = 0; j < 4; j++) {
                            int x = (int)((v << (16 + 4 * j)) & 0xF0000000u) >> 28; /* 4-bit two's complement */
                            if (cb >= 3 && x && br_get(b, 1))
                                x = -x;
                            o[i + j] = x;
                        }
                    }
                } else {
                    for (int i = 0; i < width; i += 2) {
                        uint32_t v = huff(b, t);
                        int y = (int8_t)(v >> 8), z = (int8_t)v;
                        int sy = 0, sz = 0;
                        if (cb >= 7) {
                            if (y)
                                sy = br_get(b, 1);
                            if (z)
                                sz = br_get(b, 1);
                        }
                        if (cb == ESC_HCB) {
                            STAT(escapes, (y == 16) + (z == 16));
                            if (y == 16 && (y = escape(b)) < 0)
                                return AAC_EDATA;
                            if (z == 16 && (z = escape(b)) < 0)
                                return AAC_EDATA;
                        }
                        o[i] = sy ? -y : y;
                        o[i + 1] = sz ? -z : z;
                    }
                }
                if (br_over(b))
                    return AAC_EBITS;
            }
        }
        win += s->group_len[g];
    }
    return 0;
}

/* sign(q) * |q|^(4/3) * 2^(sf/4), Q2 */
static void dequant(const struct aac_ics *s, const int16_t *q, int32_t *x) {
    dsp_zero(x, AAC_FRAME * sizeof *x);
    int win = 0;
    for (int g = 0; g < s->num_groups; g++) {
        for (int k = 0; k < s->max_sfb; k++) {
            int cb = s->cb[g][k];
            if (cb == ZERO_HCB || cb >= NOISE_HCB)
                continue;
            int g2 = s->sf[g][k] - 100, e = g2 >> 2;
            uint32_t frac = aac_pow2_frac[g2 & 3];
            int sh = 13 - e; /* (Q17 * Q30) >> 32 = Q15; Q2 needs >> 13, times 2^e */
            int start = s->swb[k], end = s->swb[k + 1];
            for (int w = 0; w < s->group_len[g]; w++) {
                int base = (win + w) * 128;
                for (int i = start; i < end; i++) {
                    int v = q[base + i];
                    if (!v)
                        continue;
                    unsigned a = v < 0 ? -v : v;
                    if (a > 8191) /* an escape's 8191 plus a pulse: past the table's interpolation */
                        a = 8191;
                    uint32_t m;
                    int ash = sh;
                    if (a < 1024) {
                        m = aac_pow43[a];
                    } else { /* a^(4/3) = 16 (a/8)^(4/3): the table's Q17 is Q13 here */
                        unsigned j = a >> 3;
                        m = aac_pow43[j] + (((aac_pow43[j + 1] - aac_pow43[j]) * (a & 7)) >> 3);
                        ash -= 4;
                    }
                    uint32_t q15 = (uint32_t)(((u64)m * frac) >> 32); /* < 2^31 */
                    int32_t r;
                    if (ash >= 31)
                        r = 0;
                    else if (ash > 0)
                        r = (int32_t)((q15 + (1u << (ash - 1))) >> ash);
                    else
                        r = clip30((i64)q15 << (ash < -31 ? 31 : -ash));
                    x[base + i] = v < 0 ? -r : r;
                }
            }
        }
        win += s->group_len[g];
    }
}

static void apply_pulses(const struct aac_ics *s, int16_t *q) {
    STAT(pulses, s->pulse_n);
    int k = s->swb[s->pulse_start];
    for (int i = 0; i < s->pulse_n; i++) {
        k += s->pulse_off[i];
        if (k >= AAC_FRAME)
            return;
        q[k] += q[k] > 0 ? s->pulse_amp[i] : -(int)s->pulse_amp[i];
    }
}

/* noise substitution: each band's random vector scaled to energy 2^(nrg/2); a seed per band (for the right channel
 * of a pair with correlated noise) */
static void pns(struct aac_dec *d, int ch, int32_t *x, int correlated) {
    const struct aac_ics *s = &d->ics[ch];
    int win = 0;
    for (int g = 0; g < s->num_groups; g++) {
        for (int k = 0; k < s->max_sfb; k++) {
            if (s->cb[g][k] != NOISE_HCB)
                continue;
            STAT(pns_bands, 1);
            uint32_t seed;
            int same = correlated && d->ics[0].cb[g][k] == NOISE_HCB && d->ms[g][k];
            if (same)
                seed = d->noise_seed[g][k];
            else {
                seed = d->seed;
                if (ch == 0)
                    d->noise_seed[g][k] = seed;
            }
            int nrg = s->sf[g][k], e = nrg >> 2;
            uint32_t frac = aac_pow2_frac[nrg & 3];
            int start = s->swb[k], width = s->swb[k + 1] - start;
            for (int w = 0; w < s->group_len[g]; w++) {
                int32_t *o = x + (win + w) * 128 + start;
                u64 energy = 0;
                for (int i = 0; i < width; i++) {
                    seed = seed * 1664525u + 1013904223u;
                    o[i] = (int32_t)seed >> 16;
                    energy += (u64)((i64)o[i] * o[i]);
                }
                uint32_t root = isqrt64(energy);
                if (!root)
                    continue;
                u64 inv = div64_32((u64)1 << 46, root);
                int sh = 58 - e; /* r * frac >> 16 * inv -> Q2; |r| <= root: < 2^61 */
                for (int i = 0; i < width; i++) {
                    i64 t = (((i64)o[i] * frac) >> 16) * (i64)inv;
                    o[i] = sh >= 63 ? 0 : sh <= 0 ? (t < 0 ? -(1 << 30) : (1 << 30)) : clip30(t >> sh);
                }
            }
            if (!same)
                d->seed = seed;
        }
        win += s->group_len[g];
    }
}

static void mid_side(struct aac_dec *d) {
    const struct aac_ics *s = &d->ics[0];
    int win = 0;
    for (int g = 0; g < s->num_groups; g++) {
        for (int k = 0; k < s->max_sfb; k++) {
            if (!d->ms[g][k] || d->ics[0].cb[g][k] >= NOISE_HCB || d->ics[1].cb[g][k] >= NOISE_HCB)
                continue;
            for (int w = 0; w < s->group_len[g]; w++) {
                int32_t *l = d->spec[0] + (win + w) * 128, *r = d->spec[1] + (win + w) * 128;
                for (int i = s->swb[k]; i < s->swb[k + 1]; i++) {
                    int32_t a = l[i], c = r[i];
                    l[i] = sat32((i64)a + c);
                    r[i] = sat32((i64)a - c);
                }
            }
        }
        win += s->group_len[g];
    }
}

static void intensity(struct aac_dec *d) {
    const struct aac_ics *s = &d->ics[1];
    int win = 0;
    for (int g = 0; g < s->num_groups; g++) {
        for (int k = 0; k < s->max_sfb; k++) {
            int cb = s->cb[g][k];
            if (cb != INTENSITY_HCB && cb != INTENSITY_HCB2)
                continue;
            STAT(is_bands, 1);
            int neg = cb == INTENSITY_HCB2;
            if (d->ms_present == 1 && d->ms[g][k])
                neg = !neg;
            int p = -s->sf[g][k], e = p >> 2; /* 2^(-position/4) = frac * 2^e, e in -25..38 */
            uint32_t frac = aac_pow2_frac[p & 3];
            for (int w = 0; w < s->group_len[g]; w++) {
                const int32_t *l = d->spec[0] + (win + w) * 128;
                int32_t *r = d->spec[1] + (win + w) * 128;
                for (int i = s->swb[k]; i < s->swb[k + 1]; i++) {
                    i64 v = ((i64)l[i] * frac) >> 30; /* |v| < 2^31 */
                    if (e >= 0)
                        v = e > 31 ? (v > 0 ? (i64)1 << 40 : v < 0 ? -((i64)1 << 40) : 0) : v * ((i64)1 << e);
                    else
                        v = -e > 40 ? 0 : v >> -e;
                    r[i] = clip30(neg ? -v : v);
                }
            }
        }
        win += s->group_len[g];
    }
}

/* temporal noise shaping: an all-pole filter over parts of each window's spectrum */
static void tns(const struct aac_ics *s, int32_t *x) {
    int mmm = s->tns_max < s->max_sfb ? s->tns_max : s->max_sfb;
    for (int w = 0; w < s->num_windows; w++) {
        int bottom = s->num_swb;
        for (int f = 0; f < s->tns_w[w].n_filt; f++) {
            int top = bottom, order = s->tns_w[w].f[f].order;
            bottom = top - s->tns_w[w].f[f].length;
            if (bottom < 0)
                bottom = 0;
            if (!order)
                continue;
            int32_t lpc[12]; /* Q24, from the reflection coefficients */
            for (int i = 0; i < order; i++) {
                int32_t r = -(int32_t)((s->tns_w[w].f[f].coef[i] + 64) >> 7);
                lpc[i] = r;
                for (int j = 0; j < (i + 1) >> 1; j++) {
                    int32_t fj = lpc[j], bj = lpc[i - 1 - j];
                    lpc[j] = sat32(fj + (((i64)r * bj + (1 << 23)) >> 24));
                    lpc[i - 1 - j] = sat32(bj + (((i64)r * fj + (1 << 23)) >> 24));
                }
            }
            int start = s->swb[bottom < mmm ? bottom : mmm], end = s->swb[top < mmm ? top : mmm];
            int size = end - start;
            if (size <= 0)
                continue;
            int inc = s->tns_w[w].f[f].dir ? -1 : 1, pos = (inc < 0 ? end - 1 : start) + w * 128;
            for (int m = 0; m < size; m++, pos += inc) {
                i64 acc = 0;
                int n = m < order ? m : order;
                for (int i = 1; i <= n; i++)
                    acc += (i64)x[pos - i * inc] * lpc[i - 1];
                x[pos] = clip30((i64)x[pos] - ((acc + (1 << 23)) >> 24));
            }
        }
    }
}

/* Q9 sample * Q31 window, high word: Q8 */
static inline int32_t win(int32_t v, int32_t w) { return mulhi(v, w); }
static inline int32_t half(int32_t v) { return (v >> 1) + (v & 1); } /* (v + 1) >> 1 without the overflow */

/* the channel's spectrum -> 1024 output samples (PCM Q8) in place of the spectrum; keeps the overlap */
static void synthesize(struct aac_dec *d, int ch) {
    const struct aac_ics *s = &d->ics[ch];
    int32_t *x = d->spec[ch], *y = d->buf, *ov = d->overlap[ch];
    int ps = d->prev_shape[ch], cs = s->win_shape;
    const int32_t *lp = ps ? aac_win_long_kbd : aac_win_long_sine, *lc = cs ? aac_win_long_kbd : aac_win_long_sine;
    const int32_t *sp = ps ? aac_win_short_kbd : aac_win_short_sine, *sc = cs ? aac_win_short_kbd : aac_win_short_sine;
    if (s->win_seq == ONLY_LONG) {
        /* the IMDCT's reorder, both window halves and the overlap-add in one pass over z */
        const int32_t *z = d->fft;
        if (!imdct_z(d->fft, x, 2048)) {
            for (int i = 0; i < AAC_FRAME; i++) {
                x[i] = ov[i];
                ov[i] = 0;
            }
        } else {
            DSP_IMDCT_LONG_PAIRS(z, x[p] = sat32((i64)ov[p] + win(a, lp[p])); ov[p] = win(b, lc[1023 - p]))
        }
        d->prev_shape[ch] = cs;
        return;
    }
    if (s->win_seq == EIGHT_SHORT) {
        int32_t t[256];
        dsp_zero(y, 2 * AAC_FRAME * sizeof *y);
        for (int w = 0; w < 8; w++) {
            imdct(d->fft, x + 128 * w, 256, t);
            const int32_t *rise = w ? sc : sp;
            int32_t *o = y + 448 + 128 * w;
            for (int i = 0; i < 128; i++) {
                o[i] += win(t[i], rise[i]);
                o[128 + i] += win(t[128 + i], sc[127 - i]);
            }
        }
    } else {
        imdct(d->fft, x, 2048, y);
        if (s->win_seq == LONG_STOP) {
            for (int i = 0; i < 448; i++)
                y[i] = 0;
            for (int i = 0; i < 128; i++)
                y[448 + i] = win(y[448 + i], sp[i]);
            for (int i = 576; i < 1024; i++)
                y[i] = half(y[i]);
        } else {
            for (int i = 0; i < 1024; i++)
                y[i] = win(y[i], lp[i]);
        }
        if (s->win_seq == LONG_START) {
            for (int i = 0; i < 448; i++)
                y[1024 + i] = half(y[1024 + i]);
            for (int i = 0; i < 128; i++)
                y[1024 + 448 + i] = win(y[1024 + 448 + i], sc[127 - i]);
            for (int i = 576; i < 1024; i++)
                y[1024 + i] = 0;
        } else {
            for (int i = 0; i < 1024; i++)
                y[1024 + i] = win(y[1024 + i], lc[1023 - i]);
        }
    }
    for (int i = 0; i < AAC_FRAME; i++) {
        x[i] = sat32((i64)ov[i] + y[i]);
        ov[i] = y[AAC_FRAME + i];
    }
    d->prev_shape[ch] = cs;
}

/* ---- elements ---- */

static int channel_stream(struct aac_dec *d, struct br *b, int ch, int common_window) {
    struct aac_ics *s = &d->ics[ch];
    int r, global_gain = br_get(b, 8);
    if (!common_window && (r = ics_info(d, b, s)))
        return r;
    if ((r = section_data(b, s)) || (r = scalefactors(b, s, global_gain)))
        return r;
    s->pulse = br_get(b, 1);
    if (s->pulse && (r = pulse_data(b, s)))
        return r;
    s->tns = br_get(b, 1);
    if (s->tns && (r = tns_data(b, s)))
        return r;
    if (br_get(b, 1))
        return AAC_EUNSUP; /* gain_control_data: the SSR profile */
    if ((r = spectral_data(b, s, d->q)))
        return r;
    if (s->pulse)
        apply_pulses(s, d->q);
    dequant(s, d->q, d->spec[ch]);
    return 0;
}

static void finish_channel(struct aac_dec *d, int ch) {
    if (d->ics[ch].tns)
        tns(&d->ics[ch], d->spec[ch]);
    synthesize(d, ch);
}

static int single_channel(struct aac_dec *d, struct br *b, int ch) {
    br_get(b, 4); /* element_instance_tag */
    int r = channel_stream(d, b, ch, 0);
    if (r)
        return r;
    pns(d, ch, d->spec[ch], 0);
    finish_channel(d, ch);
    return 0;
}

static int channel_pair(struct aac_dec *d, struct br *b) {
    br_get(b, 4);
    int common = br_get(b, 1), r;
    STAT(cpe_common, common);
    STAT(cpe_separate, !common);
    d->ms_present = 0;
    dsp_zero(d->ms, sizeof d->ms);
    if (common) {
        if ((r = ics_info(d, b, &d->ics[0])))
            return r;
        struct aac_ics *a = &d->ics[0], *c = &d->ics[1];
        for (unsigned i = 0; i < ICS_INFO_WORDS; i++)
            ((dsp_word_t *)c)[i] = ((const dsp_word_t *)a)[i];
        d->ms_present = br_get(b, 2);
        STAT(ms1, d->ms_present == 1);
        STAT(ms2, d->ms_present == 2);
        if (d->ms_present == 3)
            return AAC_EDATA;
        for (int g = 0; g < a->num_groups; g++)
            for (int k = 0; k < a->max_sfb; k++)
                d->ms[g][k] = d->ms_present == 2 ? 1 : d->ms_present == 1 ? br_get(b, 1) : 0;
    }
    if ((r = channel_stream(d, b, 0, common)) || (r = channel_stream(d, b, 1, common)))
        return r;
    pns(d, 0, d->spec[0], 0);
    pns(d, 1, d->spec[1], common && d->ms_present);
    if (common && d->ms_present)
        mid_side(d);
    intensity(d);
    finish_channel(d, 0);
    finish_channel(d, 1);
    return 0;
}

static void skip_bytes(struct br *b, unsigned n) {
    while (n--)
        br_get(b, 8);
}

unsigned aac_state_size(void) { return sizeof(struct aac_dec); }

int aac_init(struct aac_dec *d, unsigned sample_rate, unsigned channels) {
    static const unsigned rates[13] = {96000, 88200, 64000, 48000, 44100, 32000, 24000,
                                       22050, 16000, 12000, 11025, 8000,  7350};
    dsp_zero(d, sizeof *d);
    if (channels < 1 || channels > 2)
        return -1;
    for (int i = 0; i < 13; i++) {
        if (rates[i] == sample_rate) {
            d->sf_index = i;
            d->channels = channels;
            d->seed = 0x1F2E3D4C;
            return 0;
        }
    }
    return -1;
}

int aac_decode(struct aac_dec *d, const uint8_t *in, unsigned len, int16_t *pcm) {
    struct br b;
    br_init(&b, in, len);
    STAT(frames, 1);
    int got = 0, r; /* bit 0: channel 0 decoded, bit 1: channel 1 */
    for (;;) {
        int id = br_get(&b, 3);
        if (id == ID_END)
            break;
        switch (id) {
        case ID_SCE: {
            int ch = (got & 1) && d->channels == 2 ? 1 : 0;
            if ((r = single_channel(d, &b, ch)))
                return r;
            got |= 1 << ch;
            break;
        }
        case ID_CPE:
            if (d->channels != 2)
                return AAC_EUNSUP;
            if ((r = channel_pair(d, &b)))
                return r;
            got |= 3;
            break;
        case ID_DSE: {
            br_get(&b, 4);
            int align = br_get(&b, 1);
            unsigned n = br_get(&b, 8);
            if (n == 255)
                n += br_get(&b, 8);
            if (align)
                br_get(&b, (8 - (b.pos & 7)) & 7); /* br_get, not br_skip: it refills the cache */
            skip_bytes(&b, n);
            break;
        }
        case ID_FIL: {
            unsigned n = br_get(&b, 4);
            if (n == 15)
                n += br_get(&b, 8) - 1;
            skip_bytes(&b, n);
            break;
        }
        default: /* CCE, LFE, PCE */
            return AAC_EUNSUP;
        }
        if (br_over(&b))
            return AAC_EBITS;
    }
    if (!(got & 1))
        return AAC_EDATA;
    const int32_t *l = d->spec[0], *rr = d->spec[got & 2 ? 1 : 0];
    for (int i = 0; i < AAC_FRAME; i++) {
        i64 v = ((i64)l[i] + 128) >> 8;
        pcm[i * d->channels] = v > 32767 ? 32767 : v < -32768 ? -32768 : v;
        if (d->channels == 2) {
            v = ((i64)rr[i] + 128) >> 8;
            pcm[i * 2 + 1] = v > 32767 ? 32767 : v < -32768 ? -32768 : v;
        }
    }
    return AAC_FRAME * d->channels;
}
