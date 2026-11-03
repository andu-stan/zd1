/* Vorbis I decoder, fixed point (vorbisdec.h). Written from the Vorbis I specification (Xiph.Org, 2020-07-04);
 * where the specification leaves a choice (end of packet in the middle of a residue partition, the window of a
 * block whose flags disagree with its neighbours' sizes), it does what libvorbis does.
 *
 * Numbers. Residue vectors are int32 with `rp` fractional bits, chosen per stream from its residue books so that
 * every cascade sum fits with room for coupling; the floor curve is Q31 (the specification's inverse dB table); the
 * spectrum handed to the IMDCT is X * 2^(16 + log2 N) (module/codecs/dsp/imdct.h then returns PCM Q9); time samples are
 * PCM Q9, windows Q31 (a high-word multiply: Q8), the output rounded to 16 bits.
 *
 * Integer only. The only divisions (soft, udiv32) are in the setup; per packet, the lattice codebooks' digits come
 * from a reciprocal multiply. No libc, no malloc: the setup lives in the caller's arenas. */
#include "vorbisdec.h"
#include <stddef.h>
#include "fixed.h"
#include "imdct.h"
#include "vorbis_tables.h"

/* VORBIS_STAGE(n): where the decoder is, for a hang report (the Warp0 build stores n to its control block; a
 * no-op elsewhere). Setup: 0x1000 + book, 0x1200 floors, 0x1300 residues, 0x1400 mappings, 0x1500 values;
 * packets: 0x10 + ch floor, 0x20 + submap residue, 0x30 coupling, 0x40 + ch curve, 0x50 + ch synthesis. */
#ifndef VORBIS_STAGE
#define VORBIS_STAGE(n) ((void)0)
#endif

#ifdef VORBIS_STATS /* what a stream uses, for tests (check/) */
struct {
    uint32_t packets, short_blocks, long_blocks, transitions, floor_unused, eop, books_type1, books_type2,
        residue_type[3], coupled, saturated;
} vorbis_stats;
#define STAT(f, n) (vorbis_stats.f += (n))
#else
#define STAT(f, n) ((void)0)
#endif

#define MAX_BOOKS    256
#define MAX_ENTRIES  65536 /* per codebook (libvorbis's largest: 6561) */
#define MAX_DIM      64    /* residue vectors (libvorbis's largest: 8) */
#define MAX_FLOORS   64
#define MAX_RESIDUES 64
#define MAX_MAPPINGS 64
#define MAX_MODES    64
#define MAX_POSTS    65 /* floor 1 */
#define MAX_STEP     64 /* residue 0: partition size / dimensions */
#define MAX_CDIM     64 /* residue classbook dimensions */
#define ROOT_BITS    8  /* first-level Huffman lookup */
#define SUB_BITS     8  /* further levels, at most */

/* ---- setup structures (in the arenas) ---- */

/* A Huffman table entry: a leaf (bit 31 clear: entry in bits 0-23, codeword bits this level consumes in 24-29), a
 * link to the next level (bit 31: offset into the book's table in bits 0-23, its bits in 24-28), or nothing (bit 30:
 * a code the book doesn't have). */
#define H_LINK 0x80000000u
#define H_NONE 0xC0000000u

struct book {
    uint32_t entries, used;
    uint16_t dim;
    uint8_t lookup, root_bits; /* lookup type 0, 1, 2; root_bits 0: no used entries */
    uint8_t seq, pad[3];
    const uint32_t *huff;
    /* lookup 1: q = the lattice's values (`nq` of them, rp), digit extraction by recip; lookup 2: vals = entries x
     * dim (rp). Both computed once the residues have fixed rp; the raw multiplicands are in `mult`. */
    const int32_t *q, *vals;
    uint32_t nq, recip;
    const uint16_t *mult;
    int32_t min_m, delta_m; /* the unpacked floats: mantissa, exponent */
    int16_t min_e, delta_e;
    uint8_t value_bits, in_residue;
    uint8_t pad2[2];
};

struct floor1 {
    uint8_t partitions, classes, mult, posts;
    uint8_t part_class[32];
    uint8_t class_dim[16], class_sub[16], class_master[16];
    int16_t class_book[16][8]; /* -1: none */
    uint16_t x[MAX_POSTS + 1];
    uint8_t lo[MAX_POSTS + 1], hi[MAX_POSTS + 1], order[MAX_POSTS + 1]; /* neighbours; posts sorted by x */
    uint8_t range_bits, pad[3];
    int16_t range;
    int16_t pad2;
};

struct residue {
    uint8_t type, classes, classbook, pad;
    uint32_t begin, end, psize, partvals;
    const uint8_t *map;  /* classbook entry < partvals -> its `dim` class digits, first first */
    int16_t (*books)[8]; /* [class][pass], -1: none */
    uint8_t *step;       /* [class * 8 + pass]: psize / dim, for type 0 */
};

struct mapping {
    uint8_t submaps, steps, pad[2];
    uint8_t mag[256], ang[256];
    uint8_t mux[VORBIS_MAX_CH];
    uint8_t floor[16], residue[16];
};

struct mode {
    uint8_t blockflag, mapping;
};

struct vorbis_setup {
    struct book *books;
    struct floor1 *floors;
    struct residue *residues;
    struct mapping *maps;
    struct mode mode[MAX_MODES];
    uint16_t nbooks, nfloors, nresidues, nmaps;
    unsigned fast_used, slow_used;
};

/* ---- arenas: what the setup keeps grows from the bottom, scratch (code lengths, codewords) from the top ---- */

struct arena {
    uint8_t *lo, *hi, *end;
};

/* 8-byte units: the structures hold pointers (8 bytes on a 64-bit host, where the checks run) */
#define ALIGN 8u

static void arena_init(struct arena *a, void *p, unsigned len) {
    a->lo = (uint8_t *)(((uintptr_t)p + ALIGN - 1) & ~(uintptr_t)(ALIGN - 1));
    a->end = a->hi = (uint8_t *)(((uintptr_t)p + len) & ~(uintptr_t)(ALIGN - 1));
    if (a->hi < a->lo)
        a->hi = a->end = a->lo;
}

static void *alloc(struct arena *a, unsigned bytes) {
    bytes = (bytes + ALIGN - 1) & ~(ALIGN - 1);
    if (bytes > (unsigned)(a->hi - a->lo))
        return NULL;
    void *r = a->lo;
    a->lo += bytes;
    return r;
}

static void *scratch(struct arena *a, unsigned bytes) {
    bytes = (bytes + ALIGN - 1) & ~(ALIGN - 1);
    if (bytes > (unsigned)(a->hi - a->lo))
        return NULL;
    a->hi -= bytes;
    return a->hi;
}

struct mem {
    struct arena *fast, *slow; /* the same arena when the caller gave one block */
};

/* fast first, slow when it is full */
static void *alloc_hot(const struct mem *m, unsigned bytes) {
    void *r = alloc(m->fast, bytes);
    return r ? r : alloc(m->slow, bytes);
}

/* ---- bit reader: LSB first, zeros past the end; `pos` keeps counting, so overruns are seen ---- */

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
        b->cache |= v << b->bits;
        b->bits += 8;
    }
}

/* n <= 24 */
static inline uint32_t br_peek(struct br *b, int n) {
    br_fill(b);
    return b->cache & ((1u << n) - 1);
}
static inline void br_skip(struct br *b, int n) {
    b->cache >>= n;
    b->bits -= n;
    b->pos += n;
}

static inline uint32_t br_get(struct br *b, int n) {
    if (n <= 0)
        return 0;
    if (n > 24) {
        uint32_t lo = br_get(b, 16);
        return lo | br_get(b, n - 16) << 16;
    }
    uint32_t v = br_peek(b, n);
    br_skip(b, n);
    return v;
}

static inline int br_over(const struct br *b) { return b->pos > b->len * 8; }

static int ilog(uint32_t v) { return 32 - clz32(v); }

/* Residue sums and coupling wrap instead of overflowing (only a corrupt stream gets there; wrapping is what the
 * hardware does anyway, and keeps C's overflow rules out of it) */
static inline int32_t wadd(int32_t a, int32_t b) { return (int32_t)((uint32_t)a + (uint32_t)b); }
static inline int32_t wsub(int32_t a, int32_t b) { return (int32_t)((uint32_t)a - (uint32_t)b); }

/* ---- codebooks ---- */

/* The book's entry for the next codeword, or -1 (end of packet, or a code the book doesn't have) */
static inline int32_t decode_entry(struct br *b, const struct book *bk) {
    const uint32_t *t = bk->huff;
    int bits = bk->root_bits;
    if (!bits)
        return -1; /* no used entries (libvorbis: -1 too) */
    for (;;) {
        uint32_t e = t[br_peek(b, bits)];
        if (!(e & H_LINK)) {
            br_skip(b, (e >> 24) & 63);
            return br_over(b) ? -1 : (int32_t)(e & 0xFFFFFF);
        }
        if (e == H_NONE)
            return -1;
        br_skip(b, bits);
        t = bk->huff + (e & 0xFFFFFF);
        bits = (e >> 24) & 31;
    }
}

static inline uint32_t mask32(int n) { return n >= 32 ? 0xFFFFFFFFu : (1u << n) - 1; }

static uint32_t bitrev32(uint32_t v, int n) {
    uint32_t r = 0;
    for (int i = 0; i < n; i++) {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    return r;
}

/* Fill a table of 2^bits entries (at huff[at]) for the codewords whose first `skip` bits (stream order) are
 * `prefix`, allocating further levels from the arena. codes: the codewords bit-reversed (stream order). */
static int build_level(struct arena *a, uint32_t *huff, uint32_t at, int bits, int skip, uint32_t prefix,
                       const uint8_t *len, const uint32_t *codes, uint32_t entries, uint32_t *size) {
    uint32_t n = 1u << bits, *t = huff + at;
    for (uint32_t i = 0; i < n; i++)
        t[i] = H_NONE;
    for (uint32_t e = 0; e < entries; e++) {
        int l = len[e];
        if (l <= skip || (codes[e] & mask32(skip)) != prefix)
            continue;
        uint32_t r = codes[e] >> skip; /* skip < l <= 32 */
        int rem = l - skip;
        if (rem <= bits) {
            for (uint32_t j = r; j < n; j += 1u << rem)
                t[j] = (uint32_t)rem << 24 | e;
        } else { /* longer: note the deepest remainder under this index */
            uint32_t i = r & (n - 1), deeper = (uint32_t)(rem - bits);
            uint32_t prev = (t[i] & H_LINK) && t[i] != H_NONE ? t[i] & 0xFF : 0;
            t[i] = H_LINK | (deeper > prev ? deeper : prev);
        }
    }
    for (uint32_t i = 0; i < n; i++) {
        if (!(t[i] & H_LINK) || t[i] == H_NONE)
            continue;
        int sub = (int)(t[i] & 0xFF);
        if (sub > SUB_BITS)
            sub = SUB_BITS;
        uint32_t off = *size, *next = alloc(a, 4u << sub);
        if (!next)
            return VORBIS_EMEM;
        if (next != huff + off || off > 0xFFFFFF) /* the levels are one block: offsets from its start */
            return VORBIS_EUNSUP;
        *size += 1u << sub;
        t[i] = H_LINK | (uint32_t)sub << 24 | off;
        int r = build_level(a, huff, off, sub, skip + bits, prefix | i << skip, len, codes, entries, size);
        if (r)
            return r;
    }
    return 0;
}

/* Codewords from the lengths (specification 3.2.1, as libvorbis's _make_words), then the lookup tables. */
static int build_huffman(struct arena *slow, struct book *bk, const uint8_t *len) {
    uint32_t marker[33], *codes = scratch(slow, bk->entries * 4);
    int maxlen = 0;
    if (!codes)
        return VORBIS_EMEM;
    for (int i = 0; i < 33; i++)
        marker[i] = 0; /* no memset on Warp0 */
    for (uint32_t e = 0; e < bk->entries; e++) {
        int l = len[e];
        if (!l)
            continue;
        if (l > maxlen)
            maxlen = l;
        uint32_t code = marker[l];
        if (l < 32 && (code >> l))
            return VORBIS_EDATA; /* overpopulated */
        codes[e] = bitrev32(code, l);
        for (int j = l; j > 0; j--) {
            if (marker[j] & 1) {
                marker[j] = j == 1 ? marker[1] + 1 : marker[j - 1] << 1;
                break;
            }
            marker[j]++;
        }
        for (int j = l + 1; j < 33; j++) {
            if ((marker[j] >> 1) != code)
                break;
            code = marker[j];
            marker[j] = marker[j - 1] << 1;
        }
    }
    if (bk->used != 1)
        for (int i = 1; i < 33; i++)
            if (marker[i] & mask32(i))
                return VORBIS_EDATA; /* underpopulated */
    if (!bk->used) {
        bk->root_bits = 0;
        return 0;
    }
    int root = maxlen < ROOT_BITS ? maxlen : ROOT_BITS;
    uint32_t *huff = alloc(slow, 4u << root), size = 1u << root;
    if (!huff)
        return VORBIS_EMEM;
    bk->huff = huff;
    bk->root_bits = (uint8_t)root;
    if (bk->used == 1 && maxlen == 1) { /* libvorbis: a single one-bit entry decodes from either bit */
        for (uint32_t e = 0; e < bk->entries; e++)
            if (len[e])
                huff[0] = huff[1] = 1u << 24 | e;
        return 0;
    }
    return build_level(slow, huff, 0, root, 0, 0, len, codes, bk->entries, &size);
}

/* lookup1_values: the largest r with r^dim <= entries */
static uint32_t lookup1_values(uint32_t entries, uint32_t dim) {
    uint32_t r = 1;
    for (;;) {
        u64 p = 1;
        for (uint32_t i = 0; i < dim && p <= entries; i++)
            p *= r + 1;
        if (p > entries)
            return r;
        r++;
    }
}

static void float32_unpack(uint32_t v, int32_t *m, int16_t *e) {
    int32_t mant = (int32_t)(v & 0x1FFFFF);
    int exp = (int)((v >> 21) & 0x3FF) - 788;
    if (exp > 63)
        exp = 63;
    if (exp < -63)
        exp = -63;
    *m = v & 0x80000000u ? -mant : mant;
    *e = (int16_t)exp;
}

static int read_book(struct br *b, struct arena *slow, struct book *bk) {
    if (br_get(b, 24) != 0x564342)
        return VORBIS_EDATA;
    bk->dim = (uint16_t)br_get(b, 16);
    bk->entries = br_get(b, 24);
    if (bk->entries > MAX_ENTRIES)
        return VORBIS_EUNSUP;
    uint8_t *len = scratch(slow, bk->entries);
    if (!len && bk->entries)
        return VORBIS_EMEM;
    bk->used = 0;
    if (!br_get(b, 1)) { /* unordered */
        int sparse = br_get(b, 1);
        for (uint32_t e = 0; e < bk->entries; e++) {
            len[e] = (!sparse || br_get(b, 1)) ? (uint8_t)(br_get(b, 5) + 1) : 0;
            bk->used += len[e] != 0;
        }
    } else { /* ordered */
        uint32_t e = 0;
        int l = br_get(b, 5) + 1;
        while (e < bk->entries) {
            uint32_t n = br_get(b, ilog(bk->entries - e));
            if (l > 32 || n > bk->entries - e)
                return VORBIS_EDATA;
            for (uint32_t i = 0; i < n; i++)
                len[e + i] = (uint8_t)l;
            e += n;
            l++;
        }
        bk->used = bk->entries;
    }
    if (br_over(b))
        return VORBIS_EBITS;
    int r = build_huffman(slow, bk, len);
    if (r)
        return r;
    bk->lookup = (uint8_t)br_get(b, 4);
    if (bk->lookup > 2)
        return VORBIS_EDATA;
    if (bk->lookup) {
        float32_unpack(br_get(b, 32), &bk->min_m, &bk->min_e);
        float32_unpack(br_get(b, 32), &bk->delta_m, &bk->delta_e);
        bk->value_bits = (uint8_t)(br_get(b, 4) + 1);
        bk->seq = (uint8_t)br_get(b, 1);
        if (!bk->dim)
            return VORBIS_EDATA;
        u64 n = bk->lookup == 1 ? lookup1_values(bk->entries, bk->dim) : (u64)bk->entries * bk->dim;
        if (n > MAX_ENTRIES * 8u)
            return VORBIS_EUNSUP;
        uint16_t *mult = alloc(slow, (unsigned)n * 2); /* kept until the residues fix rp */
        if (!mult && n)
            return VORBIS_EMEM;
        for (uint32_t i = 0; i < n; i++)
            mult[i] = (uint16_t)br_get(b, bk->value_bits);
        bk->mult = mult;
        bk->nq = (uint32_t)n;
    }
    return br_over(b) ? VORBIS_EBITS : 0;
}

/* log2 of a bound on |values| of the book: (2^value_bits) * |delta| + |min|, times dim if sequential */
static int book_log2_bound(const struct book *bk) {
    int d = bk->delta_m ? bk->value_bits + bk->delta_e + 21 : -64;
    int m = bk->min_m ? bk->min_e + 21 : -64;
    int l = (d > m ? d : m) + 1;
    if (bk->seq)
        l += ilog(bk->dim);
    return l;
}

/* mant * 2^(e + rp), rounded; |mant| < 2^40; saturated at 2^40 (sat32 takes it from there) */
static i64 scaled(i64 mant, int e, int rp) {
    int sh = e + rp;
    if (sh >= 0) {
        u64 a = (u64)(mant < 0 ? -mant : mant);
        if (sh >= 40 || a >= ((u64)1 << (40 - sh)))
            return mant > 0 ? (i64)1 << 40 : mant < 0 ? -((i64)1 << 40) : 0;
        return mant * ((i64)1 << sh);
    }
    sh = -sh;
    return sh >= 62 ? 0 : (mant + ((i64)1 << (sh - 1))) >> sh;
}

/* the book's values at the residue binary point */
static int book_values(const struct mem *m, struct book *bk, int rp) {
    i64 minv = scaled(bk->min_m, bk->min_e, rp);
    if (bk->lookup == 1) {
        if (bk->nq >= 65536) /* the reciprocal below; lookup 2's nq is entries * dim and may be larger */
            return VORBIS_EUNSUP;
        int32_t *q = alloc_hot(m, bk->nq * 4);
        if (!q)
            return VORBIS_EMEM;
        for (uint32_t i = 0; i < bk->nq; i++)
            q[i] = sat32(scaled((i64)bk->mult[i] * bk->delta_m, bk->delta_e, rp) + minv);
        bk->q = q;
        /* floor(x / nq) = (x * recip) >> 32 for x < 2^16 (entries <= MAX_ENTRIES), nq < 2^16 */
        bk->recip = bk->nq > 1 ? (uint32_t)(div64_32((u64)1 << 32, bk->nq) + 1) : 0;
    } else {
        int32_t *v = alloc(m->slow, bk->entries * bk->dim * 4);
        if (!v && bk->entries)
            return VORBIS_EMEM;
        for (uint32_t e = 0; e < bk->entries; e++) {
            i64 last = 0;
            for (uint32_t k = 0; k < bk->dim; k++) {
                i64 x = scaled((i64)bk->mult[e * bk->dim + k] * bk->delta_m, bk->delta_e, rp) + minv + last;
                v[e * bk->dim + k] = sat32(x);
                if (bk->seq)
                    last = x;
            }
        }
        bk->vals = v;
    }
    return 0;
}

/* The vector of entry e, element k at v[k * stride]: added in */
static inline void vq_add(const struct book *bk, uint32_t e, int32_t *v, int stride, int n) {
    if (bk->lookup == 1) {
        const int32_t *q = bk->q;
        int32_t last = 0;
        uint32_t nq = bk->nq, recip = bk->recip;
        for (int k = 0; k < n; k++) {
            uint32_t qt = nq > 1 ? (uint32_t)(((u64)e * recip) >> 32) : 0;
            int32_t x = wadd(q[nq > 1 ? e - qt * nq : 0], last);
            if (bk->seq)
                last = x;
            v[k * stride] = wadd(v[k * stride], x);
            e = qt;
        }
    } else {
        const int32_t *t = bk->vals + e * bk->dim;
        for (int k = 0; k < n; k++)
            v[k * stride] = wadd(v[k * stride], t[k]);
    }
}

/* ---- headers ---- */

static int read_floor1(struct br *b, struct floor1 *f, unsigned nbooks) {
    int maxclass = -1;
    f->partitions = (uint8_t)br_get(b, 5);
    for (int i = 0; i < f->partitions; i++) {
        f->part_class[i] = (uint8_t)br_get(b, 4);
        if (f->part_class[i] > maxclass)
            maxclass = f->part_class[i];
    }
    f->classes = (uint8_t)(maxclass + 1);
    for (int i = 0; i <= maxclass; i++) {
        f->class_dim[i] = (uint8_t)(br_get(b, 3) + 1);
        f->class_sub[i] = (uint8_t)br_get(b, 2);
        f->class_master[i] = 0;
        if (f->class_sub[i]) {
            f->class_master[i] = (uint8_t)br_get(b, 8);
            if (f->class_master[i] >= nbooks)
                return VORBIS_EDATA;
        }
        for (int j = 0; j < (1 << f->class_sub[i]); j++) {
            int bk = (int)br_get(b, 8) - 1;
            if (bk >= (int)nbooks)
                return VORBIS_EDATA;
            f->class_book[i][j] = (int16_t)bk;
        }
    }
    f->mult = (uint8_t)(br_get(b, 2) + 1);
    int rb = br_get(b, 4);
    f->x[0] = 0;
    f->x[1] = (uint16_t)(1 << rb);
    int posts = 2;
    for (int i = 0; i < f->partitions; i++) {
        int c = f->part_class[i];
        for (int j = 0; j < f->class_dim[c]; j++) {
            if (posts >= MAX_POSTS)
                return VORBIS_EDATA;
            f->x[posts++] = (uint16_t)br_get(b, rb);
        }
    }
    f->posts = (uint8_t)posts;
    static const int16_t ranges[4] = {256, 128, 86, 64};
    f->range = ranges[f->mult - 1];
    f->range_bits = (uint8_t)ilog((uint32_t)f->range - 1);
    /* sorted order (insertion: <= 65 posts), duplicates rejected (libvorbis does too), neighbours */
    for (int i = 0; i < posts; i++)
        f->order[i] = (uint8_t)i;
    for (int i = 1; i < posts; i++)
        for (int j = i; j > 0 && f->x[f->order[j - 1]] > f->x[f->order[j]]; j--) {
            uint8_t t = f->order[j];
            f->order[j] = f->order[j - 1];
            f->order[j - 1] = t;
        }
    for (int i = 1; i < posts; i++)
        if (f->x[f->order[i]] == f->x[f->order[i - 1]])
            return VORBIS_EDATA;
    for (int i = 2; i < posts; i++) {
        int lo = 0, hi = 1;
        for (int j = 0; j < i; j++) {
            if (f->x[j] < f->x[i] && f->x[j] > f->x[lo])
                lo = j;
            if (f->x[j] > f->x[i] && f->x[j] < f->x[hi])
                hi = j;
        }
        f->lo[i] = (uint8_t)lo;
        f->hi[i] = (uint8_t)hi;
    }
    return br_over(b) ? VORBIS_EBITS : 0;
}

static int read_residue(struct br *b, const struct mem *mem, struct residue *r, int type, struct book *books,
                        unsigned nbooks) {
    r->type = (uint8_t)type;
    r->begin = br_get(b, 24);
    r->end = br_get(b, 24);
    r->psize = br_get(b, 24) + 1;
    r->classes = (uint8_t)(br_get(b, 6) + 1);
    r->classbook = (uint8_t)br_get(b, 8);
    if (r->classbook >= nbooks)
        return VORBIS_EDATA;
    uint8_t cascade[64];
    for (int i = 0; i < r->classes; i++) {
        int low = br_get(b, 3), high = br_get(b, 1) ? br_get(b, 5) : 0;
        cascade[i] = (uint8_t)(high * 8 + low);
    }
    r->books = alloc_hot(mem, r->classes * 16u);
    r->step = alloc_hot(mem, r->classes * 8u);
    if (!r->books || !r->step)
        return VORBIS_EMEM;
    for (int i = 0; i < r->classes; i++)
        for (int j = 0; j < 8; j++) {
            int bk = -1;
            r->step[i * 8 + j] = 0;
            if (cascade[i] & (1 << j)) {
                bk = (int)br_get(b, 8);
                if (bk >= (int)nbooks || !books[bk].lookup || books[bk].dim > MAX_DIM)
                    return VORBIS_EDATA;
                uint32_t step = udiv32(r->psize, books[bk].dim);
                if (step * books[bk].dim != r->psize)
                    return VORBIS_EUNSUP; /* libvorbis's encoders: whole */
                if (step > MAX_STEP)
                    return VORBIS_EUNSUP;
                r->step[i * 8 + j] = (uint8_t)step;
                books[bk].in_residue = 1;
            }
            r->books[i][j] = (int16_t)bk;
        }
    /* the classbook: partvals = classes^dim entries, each a digit string */
    const struct book *cb = &books[r->classbook];
    uint32_t pv = 1;
    for (int i = 0; i < cb->dim; i++) {
        pv *= r->classes;
        if (pv > cb->entries)
            return VORBIS_EDATA;
    }
    if (!cb->dim)
        return VORBIS_EDATA;
    if (cb->dim > MAX_CDIM)
        return VORBIS_EUNSUP;
    r->partvals = pv;
    uint8_t *map = alloc_hot(mem, pv * cb->dim);
    if (!map && pv)
        return VORBIS_EMEM;
    for (uint32_t e = 0; e < pv; e++) {
        uint32_t t = e;
        for (int i = cb->dim - 1; i >= 0; i--) {
            uint32_t q = udiv32(t, r->classes);
            map[e * cb->dim + i] = (uint8_t)(t - q * r->classes);
            t = q;
        }
    }
    r->map = map;
    return br_over(b) ? VORBIS_EBITS : 0;
}

static int read_mapping(struct br *b, struct mapping *m, unsigned ch, unsigned nfloors, unsigned nresidues) {
    if (br_get(b, 16) != 0)
        return VORBIS_EDATA;
    m->submaps = (uint8_t)(br_get(b, 1) ? br_get(b, 4) + 1 : 1);
    m->steps = 0;
    if (br_get(b, 1)) {
        m->steps = (uint8_t)(br_get(b, 8) + 1); /* up to 256 */
        for (int i = 0; i < m->steps; i++) {
            m->mag[i] = (uint8_t)br_get(b, ilog(ch - 1));
            m->ang[i] = (uint8_t)br_get(b, ilog(ch - 1));
            if (m->mag[i] == m->ang[i] || m->mag[i] >= ch || m->ang[i] >= ch)
                return VORBIS_EDATA;
        }
    }
    if (br_get(b, 2))
        return VORBIS_EDATA;
    for (unsigned c = 0; c < ch; c++) {
        m->mux[c] = (uint8_t)(m->submaps > 1 ? br_get(b, 4) : 0);
        if (m->mux[c] >= m->submaps)
            return VORBIS_EDATA;
    }
    for (int i = 0; i < m->submaps; i++) {
        br_get(b, 8);
        m->floor[i] = (uint8_t)br_get(b, 8);
        m->residue[i] = (uint8_t)br_get(b, 8);
        if (m->floor[i] >= nfloors || m->residue[i] >= nresidues)
            return VORBIS_EDATA;
    }
    return br_over(b) ? VORBIS_EBITS : 0;
}

static int header_start(struct br *b, int type) {
    static const uint8_t vorbis[6] = {'v', 'o', 'r', 'b', 'i', 's'};
    if (br_get(b, 8) != (uint32_t)type)
        return VORBIS_EDATA;
    for (int i = 0; i < 6; i++)
        if (br_get(b, 8) != vorbis[i])
            return VORBIS_EDATA;
    return 0;
}

unsigned vorbis_state_size(void) { return sizeof(struct vorbis_dec); }

const struct vorbis_info *vorbis_stream_info(const struct vorbis_dec *d) { return &d->info; }

void vorbis_restart(struct vorbis_dec *d) {
    d->prev_w = -1;
    dsp_zero(d->overlap, sizeof(int32_t) * VORBIS_MAX_CH * (VORBIS_MAX_BLOCK / 2));
}

void vorbis_arena_used(const struct vorbis_dec *d, unsigned *fast, unsigned *slow) {
    *fast = d->setup ? d->setup->fast_used : 0;
    *slow = d->setup ? d->setup->slow_used : 0;
}

int vorbis_init(struct vorbis_dec *d, const uint8_t *ident, unsigned ilen, const uint8_t *setup, unsigned slen,
                void *fast_mem, unsigned fast_len, void *slow_mem, unsigned slow_len) {
    struct br b;
    struct arena fa, sa;
    struct mem m = {&fa, slow_mem == fast_mem ? &fa : &sa};
    int r;
#ifdef VORBIS_EXT_BUFFERS
    int32_t (*ov)[VORBIS_MAX_BLOCK / 2] = d->overlap;
    int32_t *yb = d->ybuf;
#endif
    dsp_zero(d, sizeof *d);
#ifdef VORBIS_EXT_BUFFERS
    d->overlap = ov;
    d->ybuf = yb;
#endif
    d->prev_w = -1;
    arena_init(&fa, fast_mem, fast_len);
    arena_init(&sa, slow_mem, slow_len);
    uint8_t *fast0 = fa.lo, *slow0 = m.slow->lo;

    /* identification */
    br_init(&b, ident, ilen);
    if ((r = header_start(&b, 1)))
        return r;
    if (br_get(&b, 32) != 0)
        return VORBIS_EUNSUP;
    struct vorbis_info *in = &d->info;
    in->channels = (uint8_t)br_get(&b, 8);
    in->rate = br_get(&b, 32);
    br_get(&b, 32);
    in->bitrate_nominal = (int32_t)br_get(&b, 32);
    br_get(&b, 32);
    int b0 = br_get(&b, 4), b1 = br_get(&b, 4);
    if (!br_get(&b, 1) || br_over(&b))
        return VORBIS_EBITS;
    if (!in->channels || !in->rate || b0 < 6 || b1 > 13 || b0 > b1)
        return VORBIS_EDATA;
    if (in->channels > VORBIS_MAX_CH || b0 != 8 || b1 != 11)
        return VORBIS_EUNSUP;
    in->blocksize[0] = (uint16_t)(1 << b0);
    in->blocksize[1] = (uint16_t)(1 << b1);

    /* setup: codebooks (their code lengths and codewords are scratch at the top of the slow arena, freed after
     * each book), time placeholders, floors, residues, mappings, modes */
    struct vorbis_setup *s = alloc(&fa, sizeof *s);
    if (!s)
        return VORBIS_EMEM;
    dsp_zero(s, sizeof *s);
    br_init(&b, setup, slen);
    if ((r = header_start(&b, 5)))
        return r;
    s->nbooks = (uint16_t)(br_get(&b, 8) + 1);
    s->books = alloc_hot(&m, s->nbooks * sizeof(struct book));
    if (!s->books)
        return VORBIS_EMEM;
    for (int i = 0; i < s->nbooks; i++) {
        dsp_zero(&s->books[i], sizeof s->books[i]);
        VORBIS_STAGE(0x1000 + i);
        r = read_book(&b, m.slow, &s->books[i]);
        m.slow->hi = m.slow->end;
        if (r)
            return r;
    }
    int ntime = br_get(&b, 6) + 1;
    for (int i = 0; i < ntime; i++)
        if (br_get(&b, 16))
            return VORBIS_EDATA;
    s->nfloors = (uint16_t)(br_get(&b, 6) + 1);
    VORBIS_STAGE(0x1200);
    s->floors = alloc_hot(&m, s->nfloors * sizeof(struct floor1));
    if (!s->floors)
        return VORBIS_EMEM;
    for (int i = 0; i < s->nfloors; i++) {
        int t = br_get(&b, 16);
        if (t == 0)
            return VORBIS_EUNSUP; /* floor 0 */
        if (t != 1)
            return VORBIS_EDATA;
        if ((r = read_floor1(&b, &s->floors[i], s->nbooks)))
            return r;
    }
    s->nresidues = (uint16_t)(br_get(&b, 6) + 1);
    VORBIS_STAGE(0x1300);
    s->residues = alloc_hot(&m, s->nresidues * sizeof(struct residue));
    if (!s->residues)
        return VORBIS_EMEM;
    for (int i = 0; i < s->nresidues; i++) {
        int t = br_get(&b, 16);
        if (t > 2)
            return VORBIS_EDATA;
        if ((r = read_residue(&b, &m, &s->residues[i], t, s->books, s->nbooks)))
            return r;
    }
    s->nmaps = (uint16_t)(br_get(&b, 6) + 1);
    VORBIS_STAGE(0x1400);
    s->maps = alloc_hot(&m, s->nmaps * sizeof(struct mapping));
    if (!s->maps)
        return VORBIS_EMEM;
    for (int i = 0; i < s->nmaps; i++)
        if ((r = read_mapping(&b, &s->maps[i], in->channels, s->nfloors, s->nresidues)))
            return r;
    in->modes = (uint8_t)(br_get(&b, 6) + 1);
    for (int i = 0; i < in->modes; i++) {
        s->mode[i].blockflag = (uint8_t)br_get(&b, 1);
        if (br_get(&b, 16) || br_get(&b, 16))
            return VORBIS_EDATA;
        s->mode[i].mapping = (uint8_t)br_get(&b, 8);
        if (s->mode[i].mapping >= s->nmaps)
            return VORBIS_EDATA;
    }
    if (!br_get(&b, 1) || br_over(&b))
        return VORBIS_EBITS;
    in->mode_bits = (uint8_t)ilog(in->modes - 1u);

    /* the residue binary point: the largest that holds every cascade sum with room to spare (coupling adds one) */
    int bound = -64;
    for (int i = 0; i < s->nresidues; i++) {
        const struct residue *rs = &s->residues[i];
        int sum = -64; /* log2 of a bound on the sum of the passes' values */
        for (int p = 0; p < 8; p++) {
            int pm = -64;
            for (int c = 0; c < rs->classes; c++)
                if (rs->books[c][p] >= 0) {
                    int l = book_log2_bound(&s->books[rs->books[c][p]]);
                    if (l > pm)
                        pm = l;
                }
            if (pm > -64)
                sum = (sum > pm ? sum : pm) + 1;
        }
        if (sum > bound)
            bound = sum;
    }
    VORBIS_STAGE(0x1500);
    d->rp = 28 - bound;
    if (d->rp > 24)
        d->rp = 24;
    if (d->rp < 0)
        d->rp = 0;
    for (int i = 0; i < s->nbooks; i++)
        if (s->books[i].in_residue && (r = book_values(&m, &s->books[i], d->rp)))
            return r;
    s->fast_used = (unsigned)(fa.lo - fast0);
    s->slow_used = m.slow == &fa ? 0 : (unsigned)(sa.lo - slow0);
    d->setup = s;
    vorbis_restart(d);
    return 0;
}

/* ---- floor 1 ---- */

static int render_point(int x0, int x1, int y0, int y1, int x) {
    y0 &= 0x7FFF;
    y1 &= 0x7FFF;
    int dy = y1 - y0, adx = x1 - x0, ady = dy < 0 ? -dy : dy;
    int off = (int)udiv32((uint32_t)(ady * (x - x0)), (uint32_t)adx);
    return dy < 0 ? y0 - off : y0 + off;
}

/* The posts of one channel's floor into fit[]; 0 if the floor is unused (or the packet ended) */
static int floor1_decode(struct br *b, const struct vorbis_setup *s, const struct floor1 *f, int16_t *fit) {
    if (!br_get(b, 1))
        return 0;
    fit[0] = (int16_t)br_get(b, f->range_bits);
    fit[1] = (int16_t)br_get(b, f->range_bits);
    int j = 2;
    for (int i = 0; i < f->partitions; i++) {
        int c = f->part_class[i], cdim = f->class_dim[c], cbits = f->class_sub[c];
        int32_t cval = 0;
        if (cbits) {
            cval = decode_entry(b, &s->books[f->class_master[c]]);
            if (cval < 0)
                return 0;
        }
        for (int k = 0; k < cdim; k++) {
            int bk = f->class_book[c][cval & ((1 << cbits) - 1)];
            cval >>= cbits;
            if (bk >= 0) {
                int32_t v = decode_entry(b, &s->books[bk]);
                if (v < 0)
                    return 0;
                fit[j + k] = (int16_t)v;
            } else {
                fit[j + k] = 0;
            }
        }
        j += cdim;
    }
    if (br_over(b))
        return 0;
    /* unwrap (libvorbis's floor1_inverse1: bit 15 marks a post whose step 2 flag is clear) */
    for (int i = 2; i < f->posts; i++) {
        int lo = f->lo[i], hi = f->hi[i];
        int pred = render_point(f->x[lo], f->x[hi], fit[lo], fit[hi], f->x[i]);
        int hiroom = f->range - pred, loroom = pred;
        int room = (hiroom < loroom ? hiroom : loroom) << 1;
        int val = fit[i];
        if (val) {
            if (val >= room)
                val = hiroom > loroom ? val - loroom : -1 - (val - hiroom);
            else
                val = val & 1 ? -((val + 1) >> 1) : val >> 1;
            fit[i] = (int16_t)((val + pred) & 0x7FFF);
            fit[lo] &= 0x7FFF;
            fit[hi] &= 0x7FFF;
        } else {
            fit[i] = (int16_t)(pred | 0x8000);
        }
    }
    return 1;
}

/* x[k] = residue * floor, for k < n: the floor curve rendered line by line (libvorbis's render_line), each
 * product scaled to the IMDCT's input (shift right by sh) */
static inline int32_t fmul(int32_t r, int y, int sh) { return sat32(((i64)r * vorbis_floor1_db[y]) >> sh); }

static void render_line(int n, int x0, int x1, int y0, int y1, int32_t *x, int sh) {
    int dy = y1 - y0, adx = x1 - x0, ady = dy < 0 ? -dy : dy;
    if (adx <= 0)
        return;
    int base = dy < 0 ? -(int)udiv32((uint32_t)ady, (uint32_t)adx) : (int)udiv32((uint32_t)ady, (uint32_t)adx);
    int sy = dy < 0 ? base - 1 : base + 1, xi = x0, y = y0, err = 0;
    ady -= (base < 0 ? -base : base) * adx;
    if (n > x1)
        n = x1;
    if (xi < n)
        x[xi] = fmul(x[xi], y, sh);
    while (++xi < n) {
        err += ady;
        if (err >= adx) {
            err -= adx;
            y += sy;
        } else
            y += base;
        x[xi] = fmul(x[xi], y, sh);
    }
}

static void floor1_apply(const struct floor1 *f, const int16_t *fit, int32_t *x, int n, int sh) {
    int hx = 0, lx = 0, ly = fit[0] * f->mult;
    ly = ly < 0 ? 0 : ly > 255 ? 255 : ly;
    for (int j = 1; j < f->posts; j++) {
        int cur = f->order[j];
        int hy = fit[cur] & 0x7FFF;
        if (hy != fit[cur])
            continue;
        hx = f->x[cur];
        hy *= f->mult;
        hy = hy < 0 ? 0 : hy > 255 ? 255 : hy;
        render_line(n, lx, hx, ly, hy, x, sh);
        lx = hx;
        ly = hy;
    }
    for (int j = hx; j < n; j++)
        x[j] = fmul(x[j], ly, sh);
}

/* ---- residues ---- */

/* Decode residue r into the vectors v[0..ch) (n each; the ones with do_not[] set are skipped, as the
 * specification says). End of packet: what was decoded stays (libvorbis). */
static void residue_decode(struct vorbis_dec *d, struct br *b, const struct residue *r, int32_t **v,
                           const uint8_t *do_not, int ch, int n) {
    const struct vorbis_setup *s = d->setup;
    const struct book *cb = &s->books[r->classbook];
    int cdim = cb->dim, type = r->type;
    int32_t *vec[VORBIS_MAX_CH];
    int nv = 0;
    if (type == 2) {
        int any = 0;
        for (int j = 0; j < ch; j++)
            any |= !do_not[j];
        if (!any)
            return;
        nv = 1;
    } else {
        for (int j = 0; j < ch; j++)
            if (!do_not[j])
                vec[nv++] = v[j];
        if (!nv)
            return;
    }
    uint32_t size = type == 2 ? (uint32_t)n * ch : (uint32_t)n;
    uint32_t begin = r->begin < size ? r->begin : size, end = r->end < size ? r->end : size;
    if (end <= begin)
        return;
    uint32_t parts = udiv32(end - begin, r->psize);
    /* the partitions' classes, per vector (decoded on pass 0, used by all passes): a codeword's worth past the
     * last partition */
    uint8_t *cls = d->cls;
    uint32_t stride = type == 2 ? 0 : VORBIS_MAX_BLOCK / 2 + MAX_CDIM;
    STAT(residue_type[type], 1);
    for (int pass = 0; pass < 8; pass++) {
        for (uint32_t i = 0; i < parts;) {
            if (pass == 0)
                for (int j = 0; j < nv; j++) {
                    int32_t t = decode_entry(b, cb);
                    if (t < 0 || (uint32_t)t >= r->partvals) {
                        STAT(eop, 1);
                        return;
                    }
                    const uint8_t *m = r->map + (uint32_t)t * cdim;
                    for (int k = 0; k < cdim; k++)
                        cls[j * stride + i + k] = m[k];
                }
            for (int k = 0; k < cdim && i < parts; k++, i++)
                for (int j = 0; j < nv; j++) {
                    int c = cls[j * stride + i], bkn = r->books[c][pass];
                    if (bkn < 0)
                        continue;
                    const struct book *bk = &s->books[bkn];
                    if (!bk->used)
                        continue;
                    uint32_t off = begin + i * r->psize;
                    int dim = bk->dim;
                    if (type == 0) { /* interleaved: element k of vector t at off + t + k * step */
                        int step = r->step[c * 8 + pass];
                        int32_t e[MAX_STEP];
                        for (int t = 0; t < step; t++) {
                            e[t] = decode_entry(b, bk);
                            if (e[t] < 0) {
                                STAT(eop, 1);
                                return;
                            }
                        }
                        for (int t = 0; t < step; t++)
                            vq_add(bk, (uint32_t)e[t], vec[j] + off + t, step, dim);
                    } else if (type == 1) {
                        for (uint32_t t = 0; t < r->psize; t += dim) {
                            int32_t e = decode_entry(b, bk);
                            if (e < 0) {
                                STAT(eop, 1);
                                return;
                            }
                            vq_add(bk, (uint32_t)e, vec[j] + off + t, 1, dim);
                        }
                    } else { /* 2: one vector interleaving the channels */
                        for (uint32_t t = 0; t < r->psize; t += dim) {
                            int32_t e = decode_entry(b, bk);
                            if (e < 0) {
                                STAT(eop, 1);
                                return;
                            }
                            if (ch == 1) {
                                vq_add(bk, (uint32_t)e, v[0] + off + t, 1, dim);
                            } else { /* element p of the interleaved vector: channel p & 1, sample p >> 1 */
                                int32_t tmp[MAX_DIM];
                                for (int k2 = 0; k2 < dim; k2++)
                                    tmp[k2] = 0;
                                vq_add(bk, (uint32_t)e, tmp, 1, dim);
                                uint32_t p = off + t;
                                for (int k2 = 0; k2 < dim; k2++, p++)
                                    v[p & 1][p >> 1] = wadd(v[p & 1][p >> 1], tmp[k2]);
                            }
                        }
                    }
                }
        }
    }
}

/* ---- synthesis ---- */

static inline int16_t pcm16(int32_t q8) {
    int32_t v = (q8 >> 8) + ((q8 >> 7) & 1); /* (q8 + 128) >> 8 without overflowing */
    return (int16_t)(v > 32767 ? 32767 : v < -32768 ? -32768 : v);
}

/* q9 * w (Q31): Q8 */
static inline int32_t win(int32_t v, int32_t w) { return mulhi(v, w); }
static inline int32_t half(int32_t v) { return (v >> 1) + (v & 1); } /* (v + 1) >> 1 */

/* One channel's block: IMDCT of spec (n = the block's half size), overlap with the previous block's second half,
 * out = prev/4 + cur/4 samples at pcm[0], pcm[ch], ...; the second half kept for the next block. */
static int synth(struct vorbis_dec *d, int c, int w, int16_t *pcm, int ch) {
    int32_t *x = d->spec[c], *ov = d->overlap[c], *z = d->z;
    int n = d->info.blocksize[w] / 2, n0 = d->info.blocksize[0] / 2, n1 = d->info.blocksize[1] / 2;
    int pw = d->prev_w;
    if (pw < 0) { /* the first block: only its second half is kept */
        int32_t *y = d->ybuf;
        imdct(z, x, 2 * n, y);
        for (int i = 0; i < n; i++)
            ov[i] = y[n + i];
        return 0;
    }
    if (pw == 1 && w == 1) { /* long after long, the common case: the IMDCT's reorder, both window halves and the
                                overlap-add in one pass over z */
        const int32_t *wr = vorbis_win_1024;
        int16_t *o = pcm;
        if (!imdct_z(z, x, 2048)) {
            for (int p = 0; p < 1024; p++) {
                o[p * ch] = pcm16(win(ov[p], wr[1023 - p]));
                ov[p] = 0;
            }
            return 1024;
        }
        DSP_IMDCT_LONG_PAIRS(z, o[p * ch] = pcm16(win(ov[p], wr[1023 - p]) + win(a, wr[p])); ov[p] = b)
        return 1024;
    }
    /* the rest (libvorbis's vorbis_synthesis_blockin): the overlap is as long as the shorter block's half */
    int32_t *y = d->ybuf;
    imdct(z, x, 2 * n, y);
    STAT(transitions, pw != w);
    const int32_t *w0 = vorbis_win_128;
    int out = 0;
    if (pw == 1) { /* long, then short */
        int a = n1 / 2 - n0 / 2;
        for (int i = 0; i < a; i++)
            pcm[out++ * ch] = pcm16(half(ov[i]));
        for (int i = 0; i < n0; i++)
            pcm[out++ * ch] = pcm16(win(ov[a + i], w0[n0 - 1 - i]) + win(y[i], w0[i]));
    } else if (w == 1) { /* short, then long */
        int a = n1 / 2 - n0 / 2;
        for (int i = 0; i < n0; i++)
            pcm[out++ * ch] = pcm16(win(ov[i], w0[n0 - 1 - i]) + win(y[a + i], w0[i]));
        for (int i = n0; i < n1 / 2 + n0 / 2; i++)
            pcm[out++ * ch] = pcm16(half(y[a + i]));
    } else { /* short, short */
        for (int i = 0; i < n0; i++)
            pcm[out++ * ch] = pcm16(win(ov[i], w0[n0 - 1 - i]) + win(y[i], w0[i]));
    }
    for (int i = 0; i < n; i++)
        ov[i] = y[n + i];
    return out;
}

int vorbis_decode(struct vorbis_dec *d, const uint8_t *pkt, unsigned len, int16_t *pcm) {
    const struct vorbis_setup *s = d->setup;
    struct br b;
    if (!s)
        return VORBIS_EDATA;
    br_init(&b, pkt, len);
    if (!len || br_get(&b, 1))
        return VORBIS_ENOTAUDIO;
    uint32_t m = br_get(&b, d->info.mode_bits);
    if (m >= d->info.modes || br_over(&b))
        return VORBIS_EDATA;
    const struct mode *md = &s->mode[m];
    int w = md->blockflag, n = d->info.blocksize[w] / 2, ch = d->info.channels;
    if (w)
        br_get(&b, 2); /* previous / next window flags: libvorbis uses the actual sizes */
    const struct mapping *mp = &s->maps[md->mapping];
    STAT(packets, 1);
    STAT(short_blocks, !w);
    STAT(long_blocks, w);

    uint8_t used[VORBIS_MAX_CH], do_not[VORBIS_MAX_CH];
    for (int c = 0; c < ch; c++) {
        const struct floor1 *f = &s->floors[mp->floor[mp->mux[c]]];
        VORBIS_STAGE(0x10 + c);
        used[c] = (uint8_t)floor1_decode(&b, s, f, d->fit[c]);
        STAT(floor_unused, !used[c]);
        do_not[c] = !used[c];
        dsp_zero(d->spec[c], (unsigned)n * 4);
    }
    for (int i = 0; i < mp->steps; i++)
        if (!do_not[mp->mag[i]] || !do_not[mp->ang[i]])
            do_not[mp->mag[i]] = do_not[mp->ang[i]] = 0;
    for (int sm = 0; sm < mp->submaps; sm++) {
        int32_t *v[VORBIS_MAX_CH];
        uint8_t dn[VORBIS_MAX_CH];
        int nch = 0;
        for (int c = 0; c < ch; c++)
            if (mp->mux[c] == sm) {
                v[nch] = d->spec[c];
                dn[nch++] = do_not[c];
            }
        VORBIS_STAGE(0x20 + sm);
        // cppcheck-suppress uninitvar ; dn[0..nch) is set above
        residue_decode(d, &b, &s->residues[mp->residue[sm]], v, dn, nch, n);
    }
    VORBIS_STAGE(0x30);
    for (int i = mp->steps - 1; i >= 0; i--) {
        int32_t *mv = d->spec[mp->mag[i]], *av = d->spec[mp->ang[i]];
        STAT(coupled, 1);
        for (int k = 0; k < n; k++) {
            int32_t M = mv[k], A = av[k];
            if (M > 0) {
                if (A > 0) {
                    av[k] = wsub(M, A);
                } else {
                    av[k] = M;
                    mv[k] = wadd(M, A);
                }
            } else {
                if (A > 0) {
                    av[k] = wadd(M, A);
                } else {
                    av[k] = M;
                    mv[k] = wsub(M, A);
                }
            }
        }
    }
    /* floor * residue, scaled to X * 2^(16 + log2 N): shift = 31 + rp - 16 - log2 N */
    int sh = 15 + d->rp - (w ? 11 : 8);
    for (int c = 0; c < ch; c++) {
        VORBIS_STAGE(0x40 + c);
        if (!used[c]) {
            dsp_zero(d->spec[c], (unsigned)n * 4);
            continue;
        }
        floor1_apply(&s->floors[mp->floor[mp->mux[c]]], d->fit[c], d->spec[c], n, sh);
    }
    int out = 0;
    for (int c = 0; c < ch; c++) {
        VORBIS_STAGE(0x50 + c);
        out = synth(d, c, w, pcm + c, ch);
    }
    d->prev_w = w;
    return out;
}
