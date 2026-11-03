/* Warp0 Vorbis image (W0_IMAGE_VORBIS): the Vorbis decoder (module/codecs/vorbis) behind the job ring (w0abi.h:
 * VORBIS_INIT, VORBIS_DECODE, VORBIS_RESTART), served by the runtime's job loop (rt.c). Spotify's codec.
 *
 * Laid out like the AAC image (fw/w0vorbis.ld): code, cold tables and .bss in SDRAM at W0_VORBIS_SDRAM (code
 * through the 4 KB I-cache); in local RAM the decoder's working set (residue vectors, IMDCT scratch), the tables
 * every packet reads in full (twiddles, windows, the floor's dB table), the packet being decoded and the setup's
 * small structures (codebook descriptors, lattice values, residue maps: the "fast" arena). The Huffman tables (the
 * "slow" arena), the overlap and the buffer for blocks next to a block of the other size are in SDRAM, and the
 * samples go straight to the job's output buffer (a halfword store each, ~0.4 % of Warp0 for a stereo stream). */
#include "rt.h"
#include "../../codecs/vorbis/vorbisdec.h"

#define LRAM __attribute__((section(".bss.lram"), aligned(8)))

#define PKT_LOCAL 4096 /* packets up to this are copied in; longer ones read in place */

static struct vorbis_dec dec LRAM;
static unsigned char pkt[PKT_LOCAL] LRAM;
static unsigned char fast[VORBIS_FAST_ARENA] LRAM;
static int32_t overlap[VORBIS_MAX_CH][VORBIS_MAX_BLOCK / 2]; /* SDRAM .bss */
static int32_t ybuf[VORBIS_MAX_BLOCK];
extern unsigned char _heap_start[], _heap_end[]; /* the rest of the image's SDRAM: the slow arena (w0vorbis.ld) */
static int ready;

static u32 run(volatile u32 *d) {
    u32 len = d[D_LEN];
    switch (d[D_OP]) {
    case W0_OP_NOP:
        return W0_OK;
    case W0_OP_VORBIS_INIT: {
        u32 slen = d[D_AADLEN];
        ready = 0;
        d[D_OUTLEN] = 0;
        if (len > W0_VORBIS_MAX_HEADER || slen > W0_VORBIS_MAX_HEADER)
            return W0_ELEN;
        W0_CHECK(d[D_SRC], len);
        W0_CHECK(d[D_AAD], slen);
        w0_stage(W0_STAGE_SETUP);
        dec.overlap = overlap;
        dec.ybuf = ybuf;
        int r = vorbis_init(&dec, (const unsigned char *)d[D_SRC], len, (const unsigned char *)d[D_AAD], slen, fast,
                            sizeof fast, _heap_start, (unsigned)((uintptr_t)_heap_end - (uintptr_t)_heap_start));
        d[D_CTR] = (u32)r;
        if (r)
            return W0_EFMT;
        unsigned fu, su;
        vorbis_arena_used(&dec, &fu, &su);
        d[D_OUTLEN] = su;
        d[D_TAG] = fu; /* (informational: the fast arena's use) */
        ready = 1;
        return W0_OK;
    }
    case W0_OP_VORBIS_RESTART:
        if (ready)
            vorbis_restart(&dec);
        return W0_OK;
    case W0_OP_VORBIS_DECODE: {
        d[D_OUTLEN] = 0;
        if (len > W0_VORBIS_MAX_PACKET)
            return W0_ELEN;
        W0_CHECK(d[D_SRC], (len + 3) & ~3u);
        W0_CHECK(d[D_DST], W0_VORBIS_OUT);
        if (!ready)
            return W0_EFMT;
        const unsigned char *p = (const unsigned char *)d[D_SRC];
        if (len <= PKT_LOCAL) {
            w0_stage(W0_STAGE_COPY_IN);
            copy_words((u32 *)pkt, (const u32 *)p, (len + 3) >> 2);
            p = pkt;
        }
        w0_stage(W0_STAGE_DECODE);
        int n = vorbis_decode(&dec, p, len, (short *)d[D_DST]);
        if (n < 0) {
            d[D_CTR] = (u32)n;
            return W0_EDECODE;
        }
        d[D_OUTLEN] = (u32)n * dec.info.channels * 2;
        return W0_OK;
    }
    }
    return W0_EOP;
}

static const struct w0_image image = {W0_IMAGE_VORBIS, W0_VORBIS_OPS, W0_VORBIS_SDRAM, 0, run};

void main(void) { w0_serve(&image); }
