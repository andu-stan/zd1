/* ALAC (Apple Lossless) frame decoder for Warp0: 16- and 24-bit samples, one or two channels. A port of
 * shairplay-rust's `codec/alac.rs` (itself after David Hammerton's alac.c), bit-exact with it on valid streams. Frames
 * carry no state between them (the predictor adapts within a frame), so every packet is an independent job.
 * A 24-bit frame (Apple's encoder: always) has its low byte per sample shifted off and stored uncompressed ahead of
 * the residuals; rather than hold them (2 KB a frame), the decoder skips them and reads them again at the output.
 *
 * Written for a core without a divider, `clz` or a data cache: every index is bounded (the input is whatever a
 * sender put in a packet), shifts are masked the way Rust's release build masks them, and the input is read from
 * local RAM. Where the Rust decoder would panic or return a stale buffer on a malformed frame, this one returns 0
 * samples (the frame is dropped). Also builds on the host for the golden-vector test (`fw/alac_test.c`). */
#include "alac.h"
#include "fixed.h"

typedef unsigned int u32;
typedef int s32;
typedef long long s64;
typedef unsigned char u8;

#define RICE_THRESHOLD 8

/* Rust's release arithmetic: i32 operations wrap (signed overflow is undefined in C) */
static inline s32 wadd(s32 a, s32 b) { return (s32)((u32)a + (u32)b); }
static inline s32 wsub(s32 a, s32 b) { return (s32)((u32)a - (u32)b); }
static inline s32 wmul(s32 a, s32 b) { return (s32)((u32)a * (u32)b); }

struct bits {
    const u8 *buf;
    u32 len, pos, bit;
};

static u32 byte_at(const struct bits *b, u32 p) { return p < b->len ? b->buf[p] : 0; }

static u32 readbits_16(struct bits *b, u32 n) {
    u32 p = b->pos;
    u32 r = byte_at(b, p) << 16 | byte_at(b, p + 1) << 8 | byte_at(b, p + 2);
    r = (r << b->bit) & 0x00FFFFFF;
    r >>= (24 - n) & 31;
    u32 acc = b->bit + n;
    b->pos += acc >> 3;
    b->bit = acc & 7;
    return r;
}

static u32 readbits(struct bits *b, u32 n) {
    u32 r = 0;
    if (n > 16) {
        n -= 16;
        r = readbits_16(b, 16) << (n & 31);
    }
    return r | readbits_16(b, n);
}

static u32 readbit(struct bits *b) {
    u32 r = ((byte_at(b, b->pos) << b->bit) >> 7) & 1;
    u32 acc = b->bit + 1;
    b->pos += acc >> 3;
    b->bit = acc & 7;
    return r;
}

static void unreadbits(struct bits *b, u32 n) {
    u32 total = b->pos * 8 + b->bit;
    total = total >= n ? total - n : 0;
    b->pos = total >> 3;
    b->bit = total & 7;
}

static s32 sign_extend(s32 v, u32 bits) {
    u32 s = (32 - bits) & 31;
    return (s32)((u32)v << s) >> s;
}

static s32 decode_value(struct bits *b, u32 read_sample_size, u32 k, u32 kmask) {
    u32 x = 0;
    while (x <= RICE_THRESHOLD && readbit(b))
        x++;
    if (x > RICE_THRESHOLD) {
        x = readbits(b, read_sample_size) & (0xFFFFFFFFu >> ((32 - read_sample_size) & 31));
    } else if (k != 1) {
        u32 extra = readbits(b, k);
        x *= ((1u << (k & 31)) - 1) & kmask;
        if (extra > 1)
            x += extra - 1;
        else
            unreadbits(b, 1);
    }
    return (s32)x;
}

struct rice {
    u32 initial_history, k_modifier, history_mult, k_modifier_mask;
};

static void rice_decode(struct bits *b, s32 *out, u32 n, u32 read_sample_size, const struct rice *r) {
    s32 history = (s32)r->initial_history;
    s32 sign_modifier = 0;
    for (u32 i = 0; i < n; i++) {
        s32 v = 31 - (s32)r->k_modifier - (s32)clz32((u32)((history >> 9) + 3));
        u32 k = v < 0 ? (u32)(v + (s32)r->k_modifier) : r->k_modifier;
        s32 d = wadd(decode_value(b, read_sample_size, k, 0xFFFFFFFF), sign_modifier);
        s32 t = wadd(d, 1);
        s32 h = wadd(t, (s32)((u32)t >> 31)) >> 1; /* (d + 1) / 2, truncating like Rust's */
        out[i] = (d & 1) ? wsub(0, h) : h;
        sign_modifier = 0;
        history = wsub(wadd(history, wmul(d, (s32)r->history_mult)), wmul(history, (s32)r->history_mult) >> 9);
        if (d > 0xFFFF)
            history = 0xFFFF;
        if (history < 128 && i + 1 < n) {
            sign_modifier = 1;
            u32 kk = clz32((u32)history) + (((u32)history + 16) >> 6) - 24;
            s32 block = decode_value(b, 16, kk, r->k_modifier_mask);
            if (block > 0) {
                u32 end = i + 1 + (u32)block;
                if (end > n || end < i)
                    end = n;
                for (u32 j = i + 1; j < end; j++)
                    out[j] = 0;
                i = end - 1;
            }
            if (block > 0xFFFF)
                sign_modifier = 0;
            history = 0;
        }
    }
}

static s32 sign_only(s32 v) { return v < 0 ? -1 : v > 0; }

/* 0, or -1 for a frame with more coefficients than samples (the Rust decoder panics there): not a frame */
static int predict(const s32 *err, s32 *out, u32 n, u32 rss, short *coef, u32 num, u32 quant) {
    if (!n)
        return 0;
    out[0] = err[0];
    if (num == 0) {
        for (u32 i = 1; i < n; i++)
            out[i] = err[i];
        return 0;
    }
    if (num == 31) {
        for (u32 i = 0; i + 1 < n; i++)
            out[i + 1] = sign_extend(wadd(out[i], err[i + 1]), rss);
        return 0;
    }
    if (num + 1 > n)
        return -1;
    for (u32 i = 0; i < num; i++)
        out[i + 1] = sign_extend(wadd(out[i], err[i + 1]), rss);
    unsigned long long round = 1ull << ((quant - 1) & 63); /* quant 0: Rust's masked shift, 1 << 63 */
    for (u32 off = 0; off + num + 1 < n; off++) {
        s32 ev = err[off + num + 1];
        s32 base = out[off];
        s64 sum = 0;
        for (u32 j = 0; j < num; j++)
            sum += (s64)wsub(out[off + num - j], base) * coef[j];
        s32 o = (s32)((s64)(round + (unsigned long long)sum) >> (quant & 63)); /* only the low 32 bits survive */
        out[off + num + 1] = sign_extend(wadd(wadd(o, base), ev), rss);
        if (ev > 0) {
            for (s32 pn = (s32)num - 1; pn >= 0 && ev > 0; pn--) {
                s32 val = wsub(base, out[off + num - pn]);
                s32 sign = sign_only(val);
                coef[pn] -= (short)sign;
                val = wmul(val, sign);
                ev = wsub(ev, wmul(val >> (quant & 31), (s32)num - pn));
            }
        } else if (ev < 0) {
            for (s32 pn = (s32)num - 1; pn >= 0 && ev < 0; pn--) {
                s32 val = wsub(base, out[off + num - pn]);
                s32 sign = -sign_only(val);
                coef[pn] -= (short)sign;
                val = wmul(val, sign);
                ev = wsub(ev, wmul(val >> (quant & 31), (s32)num - pn));
            }
        }
    }
    return 0;
}

struct pred_hdr {
    u32 quant, ricemod, num;
    short coef[32];
};

static void read_pred_hdr(struct bits *b, struct pred_hdr *h) {
    readbits(b, 4);
    h->quant = readbits(b, 4);
    h->ricemod = readbits(b, 3);
    h->num = readbits(b, 5);
    for (u32 i = 0; i < h->num; i++)
        h->coef[i] = (short)readbits(b, 16);
}

static s32 err_a[ALAC_MAX_FRAME], err_b[ALAC_MAX_FRAME], out_a[ALAC_MAX_FRAME], out_b[ALAC_MAX_FRAME];

static int decode_channel(struct bits *b, const struct alac_config *c, s32 *err, s32 *out, u32 n, u32 rss,
                          struct pred_hdr *h) {
    struct rice r;
    r.initial_history = c->mb;
    r.k_modifier = c->kb;
    r.history_mult = (h->ricemod * c->pb) >> 2;
    r.k_modifier_mask = (1u << (c->kb & 31)) - 1;
    rice_decode(b, err, n, rss, &r);
    return predict(err, out, n, rss, h->coef, h->num, h->quant);
}

/* the output, a word at a time: 16-bit samples paired (the first in the low half), 24-bit ones a word each */
struct out {
    unsigned *w;
    u32 wide, half, low;
};

static void put(struct out *o, s32 v) {
    if (o->wide) {
        *o->w++ = (u32)sign_extend(v, 24);
    } else if (o->half) {
        *o->w++ = o->low | (u32)v << 16;
        o->half = 0;
    } else {
        o->low = (u32)v & 0xFFFF;
        o->half = 1;
    }
}

int alac_decode(const struct alac_config *c, const unsigned char *in, unsigned len, unsigned *out) {
    if ((c->sample_size != 16 && c->sample_size != 24) || c->channels < 1 || c->channels > 2 || c->frame_length == 0 ||
        c->frame_length > ALAC_MAX_FRAME)
        return -1;
    u32 bits = c->sample_size;
    struct bits b = {in, len, 0, 0};
    u32 element = readbits(&b, 3);
    if (element > 1)
        return 0;
    u32 stereo = element == 1;
    u32 n = c->frame_length;
    readbits(&b, 4);
    readbits(&b, 12);
    u32 has_size = readbits(&b, 1);
    u32 ub = readbits(&b, 2); /* bytes shifted off (16-bit: read and ignored, as the Rust) */
    u32 verbatim = readbits(&b, 1);
    if (has_size)
        n = readbits(&b, 32);
    if (n == 0 || n > c->frame_length || ub * 8 >= bits)
        return 0;
    u32 rss = bits - ub * 8 + stereo;
    u32 shift = 0, weight = 0;
    struct bits low = b; /* the shifted-off bytes (24-bit), read at the output */
    if (!verbatim) {
        shift = readbits(&b, 8);
        weight = readbits(&b, 8);
        struct pred_hdr ha, hb;
        read_pred_hdr(&b, &ha);
        if (stereo)
            read_pred_hdr(&b, &hb);
        low = b;
        b.pos += n * (1 + stereo) * ub; /* whole bytes: the bit offset stays */
        if (decode_channel(&b, c, err_a, out_a, n, rss, &ha) ||
            (stereo && decode_channel(&b, c, err_b, out_b, n, rss, &hb)))
            return 0;
    } else {
        ub = 0; /* an escape frame has none (Apple's decoder: bytesShifted = 0) */
        for (u32 i = 0; i < n; i++) {
            out_a[i] = sign_extend((s32)readbits(&b, bits), bits);
            if (stereo)
                out_b[i] = sign_extend((s32)readbits(&b, bits), bits);
        }
    }
    if (b.pos * 8 + b.bit > len * 8) /* read past its end: not a frame (a placeholder, a cut one) */
        return 0;
    if (stereo && c->channels != 2) /* (a mono element on a stereo stream: both channels) */
        return 0;
    struct out o = {out, bits == 24, 0, 0};
    u32 lb = ub * 8;
    for (u32 i = 0; i < n; i++) {
        s32 l = out_a[i], r = stereo ? out_b[i] : l;
        if (stereo && weight) {
            s32 mid = l, diff = r;
            r = wsub(mid, wmul(diff, (s32)weight) >> (shift & 31));
            l = wadd(r, diff);
        }
        if (lb && bits == 24) {
            l = (s32)((u32)l << lb | readbits(&low, lb));
            if (stereo)
                r = (s32)((u32)r << lb | readbits(&low, lb));
            else
                r = l;
        }
        put(&o, l);
        if (c->channels == 2)
            put(&o, r);
    }
    if (o.half)
        *o.w = o.low;
    return (int)(n * c->channels);
}
