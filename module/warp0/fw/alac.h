/* ALAC decoder for Warp0 (alac.c): 16- and 24-bit, one or two channels, frames of up to ALAC_MAX_FRAME samples. */
#ifndef ALAC_H
#define ALAC_H

#ifndef ALAC_MAX_FRAME /* the host test builds with 4096 */
#include "../w0abi.h"
#define ALAC_MAX_FRAME W0_ALAC_MAX_FRAME
#endif

/* the stream's ALACSpecificConfig fields that decoding needs (the fmtp / SETUP values), one word each */
struct alac_config {
    unsigned frame_length, sample_size, pb, mb, kb, channels;
};

/* Decodes one frame of `len` bytes into interleaved samples at `out` (frame_length * channels at most), written a
 * word at a time (it may be SDRAM): 16-bit samples two to a word, the first in the low half (as an array of shorts
 * on the little-endian Warp); 24-bit ones a word each, sign-extended. Returns the number of samples, 0 for a frame
 * that can't be decoded (drop it), -1 for a configuration this decoder doesn't handle (another depth, more than
 * two channels, frames over ALAC_MAX_FRAME). */
int alac_decode(const struct alac_config *c, const unsigned char *in, unsigned len, unsigned *out);

#endif
