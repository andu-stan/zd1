/* Warp0 FLAC image (W0_IMAGE_FLAC): the FLAC decoder (module/codecs/flac) behind the job ring (w0abi.h: FLAC_INIT,
 * FLAC_DECODE), served by the runtime's job loop (rt.c). Snapcast's codec (zd1-snapcast).
 *
 * All in local RAM, laid out like the crypto image (fw/w0.ld): code, and the decoder with its one 4608-sample
 * channel buffer (18 KB of .bss). The frames are read in place from the job's input in SDRAM, one word load per 32 bits;
 * the samples go straight to the job's output buffer. FLAC has no state between frames, so a job may hold any
 * whole frames, and the ARM can take over at any frame. */
#include "rt.h"
#include "../../codecs/flac/flacdec.h"

static struct flac_dec dec;
static int ready;

static u32 run(volatile u32 *d) {
    u32 len = d[D_LEN];
    switch (d[D_OP]) {
    case W0_OP_NOP:
        return W0_OK;
    case W0_OP_FLAC_INIT: {
        int r;
        ready = 0;
        if (len < 34 || len > 256)
            return W0_ELEN;
        W0_CHECK(d[D_SRC], (len + 3) & ~3u);
        w0_stage(W0_STAGE_SETUP);
        r = flac_init(&dec, (const unsigned char *)d[D_SRC], len);
        d[D_CTR] = (u32)r;
        if (r)
            return W0_EFMT;
        ready = 1;
        return W0_OK;
    }
    case W0_OP_FLAC_DECODE: {
        u32 used, samples;
        int r;
        d[D_OUTLEN] = 0;
        d[D_TAG] = 0;
        if (len > W0_FLAC_MAX_IN)
            return W0_ELEN;
        W0_CHECK(d[D_SRC], (len + 3) & ~3u);
        W0_CHECK(d[D_DST], W0_FLAC_OUT);
        if (!ready)
            return W0_EFMT;
        w0_stage(W0_STAGE_DECODE);
        r = flac_decode(&dec, (const unsigned char *)d[D_SRC], len, (int32_t *)d[D_DST], W0_FLAC_OUT / 4, &used,
                        &samples);
        d[D_OUTLEN] = samples * 4;
        d[D_TAG] = used;
        if (r) {
            d[D_CTR] = (u32)r;
            return W0_EDECODE;
        }
        return W0_OK;
    }
    }
    return W0_EOP;
}

static const struct w0_image image = {W0_IMAGE_FLAC, W0_FLAC_OPS, W0_END, 0, run};

void main(void) { w0_serve(&image); }
