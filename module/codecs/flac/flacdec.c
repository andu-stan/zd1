/* FLAC frame decoder (flacdec.h), from the FLAC format specification (RFC 9639). Integer only; no division, no 64-bit
 * library calls (the Warp lacks a divider and libgcc: module/warp0/build.py rejects both), no libc.
 *
 * The input is read as aligned 32-bit words (big-endian bit order, so each word is byte-swapped once): one load per
 * 32 bits, which matters on Warp0, where the input is in SDRAM (~19 cycles a load). A read past the input yields
 * zero bits and marks the reader overrun; the frame is then refused. */
#include "flacdec.h"
#include "fixed.h"

typedef uint32_t u32;
typedef int32_t i32;
typedef uint8_t u8;

#ifdef FLAC_STATS /* the host checks: which parts of the decoder a corpus exercised */
unsigned long flac_stats[FLAC_NSTATS];
#define STAT(k) (flac_stats[k]++)
#else
#define STAT(k) ((void)0)
#endif

struct br {
    const u32 *w;
    u32 nwords, idx; /* words in the input, the next one to load */
    u32 cache;       /* the next `left` bits, MSB first; the bits below them are zero */
    u32 left;
    int over; /* read past the input */
};

static u32 bswap(u32 v) { return v >> 24 | (v >> 8 & 0xFF00) | (v << 8 & 0xFF0000) | v << 24; }

static u32 load(struct br *b) {
    if (b->idx >= b->nwords) {
        b->over = 1;
        return 0;
    }
    return bswap(b->w[b->idx++]);
}

/* Position the reader at byte `at` */
static void br_seek(struct br *b, u32 at) {
    u32 skip = (at & 3) << 3;
    b->idx = at >> 2;
    b->cache = load(b) << skip;
    b->left = 32 - skip;
}

static u32 br_pos(const struct br *b) { return (b->idx << 5) - b->left; } /* bits from the start */

/* n bits, 0 <= n <= 32 */
static u32 bits(struct br *b, u32 n) {
    u32 v;
    if (n == 0)
        return 0;
    if (n <= b->left) {
        v = b->cache >> (32 - n);
        b->cache = n == 32 ? 0 : b->cache << n;
        b->left -= n;
        return v;
    }
    /* the `left` bits we have, then n - left from the next word */
    v = b->left ? b->cache >> (32 - b->left) : 0;
    n -= b->left;
    {
        u32 w = load(b);
        v = (n == 32 ? 0 : v << n) | w >> (32 - n);
        b->cache = n == 32 ? 0 : w << n;
        b->left = 32 - n;
    }
    return v;
}

static i32 sbits(struct br *b, u32 n) /* n-bit two's complement, 1 <= n <= 32 */
{
    u32 v = bits(b, n);
    return n == 32 ? (i32)v : (i32)(v << (32 - n)) >> (32 - n);
}

/* zeros up to a one, which is consumed */
static u32 unary(struct br *b) {
    u32 q = 0;
    for (;;) {
        if (b->left == 0) {
            b->cache = load(b);
            b->left = 32;
            if (b->over)
                return q;
        }
        if (b->cache == 0) { /* the rest of the cache is zeros (those below `left` too) */
            q += b->left;
            b->left = 0;
            continue;
        }
        {
            u32 z = clz32(b->cache);
            q += z;
            b->cache = z + 1 == 32 ? 0 : b->cache << (z + 1);
            b->left -= z + 1;
            return q;
        }
    }
}

static void align_byte(struct br *b) { bits(b, b->left & 7); }

/* ---- CRCs over the input bytes (the Warp is little-endian: byte i of the stream is bits (i & 3) * 8 of word i/4) ---- */

static u32 byte_at(const u32 *w, u32 i) { return w[i >> 2] >> ((i & 3) << 3) & 0xFF; }

static u32 crc8(const u32 *w, u32 from, u32 to) {
    u32 c = 0;
    for (u32 i = from; i < to; i++) {
        c ^= byte_at(w, i);
        for (int k = 0; k < 8; k++)
            c = c & 0x80 ? (c << 1 ^ 0x07) & 0xFF : c << 1 & 0xFF;
    }
    return c;
}

static u32 crc16_byte(u32 c, u32 v) {
    c ^= v << 8;
    for (int k = 0; k < 8; k++)
        c = c & 0x8000 ? (c << 1 ^ 0x8005) & 0xFFFF : c << 1 & 0xFFFF;
    return c;
}

static u32 crc16(const u32 *w, u32 from, u32 to) {
    u32 c = 0;
    for (u32 i = from; i < to; i++)
        c = crc16_byte(c, byte_at(w, i));
    return c;
}

/* ---- STREAMINFO ---- */

int flac_init(struct flac_dec *d, const uint8_t *s, uint32_t len) {
    if (len < 34)
        return FLAC_EINFO;
    d->info.min_block = (u32)s[0] << 8 | s[1];
    d->info.max_block = (u32)s[2] << 8 | s[3];
    d->info.rate = (u32)s[10] << 12 | (u32)s[11] << 4 | s[12] >> 4;
    d->info.channels = (s[12] >> 1 & 7) + 1;
    d->info.bits = ((u32)(s[12] & 1) << 4 | s[13] >> 4) + 1;
    d->frames = 0;
    if (d->info.rate == 0 || d->info.max_block < 16 || d->info.min_block < 16 || d->info.min_block > d->info.max_block)
        return FLAC_EINFO;
    if (d->info.channels > FLAC_MAX_CH || d->info.bits < 4 || d->info.bits > FLAC_MAX_BPS ||
        d->info.max_block > FLAC_MAX_BLOCK)
        return FLAC_EUNSUP;
    return 0;
}

/* ---- a subframe ---- */

static int residual(struct br *b, i32 *x, u32 n, u32 order) {
    u32 method = bits(b, 2), porder, kbits, esc;
    if (method > 1)
        return FLAC_ERESIDUAL;
    if (method)
        STAT(FLAC_S_RICE2);
    kbits = method ? 5 : 4;
    esc = method ? 31 : 15;
    porder = bits(b, 4);
    if ((n >> porder) << porder != n || (n >> porder) < order)
        return FLAC_ERESIDUAL;
    {
        u32 i = order; /* the warm-up samples come first */
        for (u32 p = 0; p < 1u << porder; p++) {
            u32 end = (p + 1) * (n >> porder), k = bits(b, kbits);
            if (k == esc) {
                u32 w = bits(b, 5);
                STAT(FLAC_S_ESCAPE);
                for (; i < end; i++)
                    x[i] = w ? sbits(b, w) : 0;
            } else {
                for (; i < end; i++) {
                    u32 q = unary(b), u = (k ? q << k | bits(b, k) : q);
                    x[i] = (i32)(u >> 1) ^ -(i32)(u & 1);
                }
            }
            if (b->over)
                return FLAC_EOVERRUN;
        }
    }
    return 0;
}

/* The prediction arithmetic wraps (u32): a valid stream never overflows, a corrupt one must not be undefined
 * behaviour (the frame's CRC-16 then refuses it). */
static void fixed(i32 *s, u32 n, u32 order) {
    u32 i, *x = (u32 *)s;
    switch (order) {
    case 1:
        for (i = 1; i < n; i++)
            x[i] += x[i - 1];
        break;
    case 2:
        for (i = 2; i < n; i++)
            x[i] += 2 * x[i - 1] - x[i - 2];
        break;
    case 3:
        for (i = 3; i < n; i++)
            x[i] += 3 * (x[i - 1] - x[i - 2]) + x[i - 3];
        break;
    case 4:
        for (i = 4; i < n; i++)
            x[i] += 4 * (x[i - 1] + x[i - 3]) - 6 * x[i - 2] - x[i - 4];
        break;
    }
}

/* (acc >> shift) for a 64-bit acc, 0 <= shift <= 31, the low 32 bits: no __ashrdi3 */
static u32 asr64(uint64_t acc, u32 shift) {
    u32 lo = (u32)acc, hi = (u32)(acc >> 32);
    return shift ? lo >> shift | hi << (32 - shift) : lo;
}

static void lpc(i32 *x, u32 n, const i32 *c, u32 order, u32 shift, int wide) {
    if (!wide) { /* every sum fits 32 bits (libFLAC's test): one mult each */
        for (u32 i = order; i < n; i++) {
            u32 s = 0;
            for (u32 j = 0; j < order; j++)
                s += (u32)c[j] * (u32)x[i - 1 - j];
            x[i] = (i32)((u32)x[i] + (u32)((i32)s >> shift));
        }
    } else {
        for (u32 i = order; i < n; i++) {
            uint64_t s = 0;
            for (u32 j = 0; j < order; j++)
                s += (uint64_t)((int64_t)c[j] * x[i - 1 - j]); /* one signed mult each; the sum wraps */
            x[i] = (i32)((u32)x[i] + asr64(s, shift));
        }
    }
}

static u32 ilog2(u32 v) { return 31 - clz32(v | 1); }

static int subframe(struct br *b, i32 *x, u32 n, u32 bps) {
    u32 type, wasted = 0;
    if (bits(b, 1))
        return FLAC_ESUBFRAME; /* the zero padding bit */
    type = bits(b, 6);
    if (bits(b, 1))
        wasted = unary(b) + 1;
    if (wasted >= bps)
        return FLAC_ESUBFRAME;
    if (wasted)
        STAT(FLAC_S_WASTED);
    bps -= wasted;
    if (type == 0) { /* CONSTANT */
        i32 v = sbits(b, bps);
        STAT(FLAC_S_CONSTANT);
        for (u32 i = 0; i < n; i++)
            x[i] = v;
    } else if (type == 1) { /* VERBATIM */
        STAT(FLAC_S_VERBATIM);
        for (u32 i = 0; i < n; i++)
            x[i] = sbits(b, bps);
    } else if (type >= 8 && type <= 12) { /* FIXED, order 0-4 */
        u32 order = type - 8, i;
        int r;
        STAT(FLAC_S_FIXED);
        if (order > n)
            return FLAC_ESUBFRAME;
        for (i = 0; i < order; i++)
            x[i] = sbits(b, bps);
        if ((r = residual(b, x, n, order)))
            return r;
        fixed(x, n, order);
    } else if (type >= 32) { /* LPC, order 1-32 */
        u32 order = type - 31, prec, i;
        i32 shift, c[32];
        int r;
        if (order > n)
            return FLAC_ESUBFRAME;
        for (i = 0; i < order; i++)
            x[i] = sbits(b, bps);
        prec = bits(b, 4) + 1;
        if (prec == 16)
            return FLAC_ESUBFRAME;
        shift = sbits(b, 5);
        if (shift < 0)
            return FLAC_ESUBFRAME;
        for (i = 0; i < order; i++)
            c[i] = sbits(b, prec);
        if ((r = residual(b, x, n, order)))
            return r;
        STAT(FLAC_S_LPC);
        if (bps + prec + ilog2(order) > 32)
            STAT(FLAC_S_LPC_WIDE);
        lpc(x, n, c, order, (u32)shift, bps + prec + ilog2(order) > 32);
    } else {
        return FLAC_ESUBFRAME;
    }
    if (b->over)
        return FLAC_EOVERRUN;
    if (wasted)
        for (u32 i = 0; i < n; i++)
            x[i] = (i32)((u32)x[i] << wasted);
    return 0;
}

/* ---- a frame ---- */

static const u32 bps_code[8] = {0, 8, 12, 0, 16, 20, 24, 32};

#define NO_ROOM 1 /* frame(): the frame doesn't fit in the output's room */

/* One frame at byte `at`, its samples into `out` (interleaved, left-justified) if it has room for them (`room`
 * samples), else NO_ROOM, nothing written; *n = its block size, *end = the byte after it. One channel buffer: channel 0
 * is decoded there and stored to `out`, channel 1 decoded there and combined with what `out` holds (the stereo
 * modes), so a frame needs 18 KB of working memory, not 36 (Warp0's local RAM is 48 KB). */
static int frame(struct flac_dec *d, struct br *b, u32 at, u32 len, u32 *out, u32 room, u32 *n, u32 *end) {
    u32 bs, rc, ca, sc, nch, bps, block;
    br_seek(b, at);
    if (bits(b, 15) != 0x7FFC) /* sync (14 bits) and the reserved zero */
        return FLAC_ESYNC;
    bits(b, 1); /* blocking strategy: fixed or variable, either is fine here */
    bs = bits(b, 4);
    rc = bits(b, 4);
    ca = bits(b, 4);
    sc = bits(b, 3);
    if (bits(b, 1) || bs == 0 || rc == 15 || ca > 10 || sc == 3)
        return FLAC_EHEADER;
    { /* the frame or sample number, UTF-8 style: skipped */
        u32 c = bits(b, 8), more = 0;
        if (c >= 0xC0) {
            u32 m = c << 24;
            while (m & 0x40000000) {
                more++;
                m <<= 1;
            }
            if (more > 6)
                return FLAC_EHEADER;
        } else if (c >= 0x80) {
            return FLAC_EHEADER;
        }
        while (more--)
            if ((bits(b, 8) & 0xC0) != 0x80)
                return FLAC_EHEADER;
    }
    if (bs == 1)
        block = 192;
    else if (bs <= 5)
        block = 576u << (bs - 2);
    else if (bs == 6)
        block = bits(b, 8) + 1;
    else if (bs == 7)
        block = bits(b, 16) + 1;
    else
        block = 256u << (bs - 8);
    if (rc == 12)
        bits(b, 8); /* the rate itself isn't needed: STREAMINFO's is the stream's */
    else if (rc >= 13)
        bits(b, 16);
    if (b->over)
        return FLAC_EOVERRUN;
    {
        u32 hdr_end = br_pos(b) >> 3, crc = bits(b, 8);
        if (b->over || hdr_end + 1 > len)
            return FLAC_EOVERRUN;
        if (crc8(b->w, at, hdr_end) != crc)
            return FLAC_ECRC8;
    }
    nch = ca < 8 ? ca + 1 : 2;
    bps = sc ? bps_code[sc] : d->info.bits;
    if (nch != d->info.channels || bps != d->info.bits)
        return FLAC_EFORMAT;
    if (block > FLAC_MAX_BLOCK)
        return FLAC_EUNSUP;
    if (block * nch > room)
        return NO_ROOM;
    {
        u32 shift = 32 - bps, *x = (u32 *)d->buf, i;
        /* the side channel has one bit more: ch 1 for left/side and mid/side, ch 0 for right/side */
        int r = subframe(b, d->buf, block, bps + (ca == 9));
        if (r)
            return r;
        if (nch != 2) { /* mono, or 3-8 independent channels */
            for (i = 0; i < block; i++)
                out[i * nch] = x[i] << shift;
            for (u32 c = 1; c < nch; c++) {
                if ((r = subframe(b, d->buf, block, bps)))
                    return r;
                for (i = 0; i < block; i++)
                    out[i * nch + c] = x[i] << shift;
            }
        } else {
            for (i = 0; i < block; i++)
                out[2 * i] = x[i]; /* channel 0 as decoded, combined below */
            if ((r = subframe(b, d->buf, block, bps + (ca == 8 || ca == 10))))
                return r;
            STAT(ca == 8    ? FLAC_S_LEFT_SIDE
                 : ca == 9  ? FLAC_S_RIGHT_SIDE
                 : ca == 10 ? FLAC_S_MID_SIDE
                            : FLAC_S_INDEPENDENT);
            for (i = 0; i < block; i++) {
                u32 a = out[2 * i], s = x[i], l, rr;
                switch (ca) {
                case 8:
                    l = a;
                    rr = a - s;
                    break; /* left / side */
                case 9:
                    l = a + s;
                    rr = s;
                    break; /* side / right */
                case 10: { /* mid / side */
                    u32 m = a << 1 | (s & 1);
                    l = (u32)((i32)(m + s) >> 1);
                    rr = (u32)((i32)(m - s) >> 1);
                    break;
                }
                default:
                    l = a;
                    rr = s;
                    break; /* independent */
                }
                out[2 * i] = l << shift;
                out[2 * i + 1] = rr << shift;
            }
        }
    }
    align_byte(b);
    {
        u32 crc_at = br_pos(b) >> 3, crc = bits(b, 16);
        if (b->over || crc_at + 2 > len)
            return FLAC_EOVERRUN;
        if (crc16(b->w, at, crc_at) != crc)
            return FLAC_ECRC16;
        *end = crc_at + 2;
    }
    *n = block;
    return 0;
}

uint32_t flac_state_size(void) { return sizeof(struct flac_dec); }

const struct flac_info *flac_stream_info(const struct flac_dec *d) { return &d->info; }

int flac_decode(struct flac_dec *d, const uint8_t *in, uint32_t len, int32_t *out, uint32_t out_cap, uint32_t *used,
                uint32_t *samples) {
    return flac_decode_from(d, in, len, 0, out, out_cap, used, samples);
}

int flac_decode_from(struct flac_dec *d, const uint8_t *in, uint32_t len, uint32_t start, int32_t *out,
                     uint32_t out_cap, uint32_t *used, uint32_t *samples) {
    struct br b = {(const u32 *)in, (len + 3) >> 2, 0, 0, 0, 0};
    u32 at = start, written = 0, ch = d->info.channels;
    *used = start;
    *samples = 0;
    while (at + 2 <= len) {
        u32 n, end;
        int r = frame(d, &b, at, len, (u32 *)out + written, out_cap - written, &n, &end);
        if (r == NO_ROOM)
            return written ? 0 : FLAC_EOUTPUT;
        if (r)
            return r;
        if (end > len)
            return FLAC_EOVERRUN;
        written += n * ch;
        d->frames++;
        at = end;
        *used = at;
        *samples = written;
    }
    return 0;
}
