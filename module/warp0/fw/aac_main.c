/* Warp0 AAC image (W0_IMAGE_AAC): the AAC-LC decoder (module/codecs/aac) behind the job ring (w0abi.h: AAC_INIT,
 * AAC_DECODE), served by the runtime's job loop (rt.c).
 *
 * A separate image from the crypto/ALAC one (main.c), because the decoder doesn't fit Warp0's 48 KB local RAM with
 * it: the code and the tables read a few times a frame (Huffman, x^(4/3), bands) run from SDRAM through the 4 KB
 * I-cache (fw/w0aac.ld, at W0_AAC_SDRAM); the decoder's working set (spectra, FFT, quantized values) and the tables
 * read in full every frame (twiddles, windows) sit in local RAM; the overlap and the buffer for start / short /
 * stop blocks are in SDRAM (read and written once a frame). */
#include "rt.h"
#include "../../codecs/aac/aacdec.h"

#define LRAM __attribute__((section(".bss.lram"), aligned(4)))

static struct aac_dec dec LRAM;
static unsigned char au[W0_AAC_MAX_AU] LRAM;
static short pcm[2 * AAC_FRAME] LRAM;
static int32_t overlap[2][AAC_FRAME]; /* SDRAM .bss */
static int32_t synth[2 * AAC_FRAME];
static uint32_t noise_seed[8][64];
static int ready;

static u32 run(volatile u32 *d) {
    u32 len = d[D_LEN];
    switch (d[D_OP]) {
    case W0_OP_NOP:
        return W0_OK;
    case W0_OP_AAC_INIT:
        ready = 0;
        if (aac_init(&dec, len, d[D_CTR]))
            return W0_EFMT;
        dec.overlap = overlap;
        dec.buf = synth;
        dec.noise_seed = noise_seed;
        for (u32 i = 0; i < 2 * AAC_FRAME; i++)
            overlap[i >> 10][i & 1023] = 0;
        ready = 1;
        return W0_OK;
    case W0_OP_AAC_DECODE: {
        d[D_OUTLEN] = 0;
        if (len > W0_AAC_MAX_AU)
            return W0_ELEN;
        W0_CHECK(d[D_SRC], (len + 3) & ~3u);
        W0_CHECK(d[D_DST], W0_AAC_OUT);
        if (!ready)
            return W0_EFMT;
        w0_stage(W0_STAGE_COPY_IN);
        copy_words((u32 *)au, (const u32 *)d[D_SRC], (len + 3) >> 2);
        w0_stage(W0_STAGE_DECODE);
        int n = aac_decode(&dec, au, len, pcm);
        if (n < 0) {
            d[D_CTR] = (u32)n;
            return W0_EDECODE;
        }
        w0_stage(W0_STAGE_COPY_OUT);
        copy_words((u32 *)d[D_DST], (const u32 *)pcm, (u32)n >> 1);
        d[D_OUTLEN] = (u32)n * 2;
        return W0_OK;
    }
    }
    return W0_EOP;
}

static const struct w0_image image = {W0_IMAGE_AAC, W0_AAC_OPS, W0_AAC_SDRAM, 0, run};

void main(void) { w0_serve(&image); }
