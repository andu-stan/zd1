/* Vorbis I decoder, fixed point (vorbisdec.c): raw packets in (the Ogg layer is the caller's), 16-bit PCM out.
 *
 * What Spotify sends (Ogg Vorbis 96-320 kb/s, 44.1 kHz stereo, block sizes 256 / 2048) and what libvorbis makes in
 * general: floor 1, residues 0/1/2, any number of codebooks, submaps and coupling steps; one or two channels, block
 * sizes 256 and 2048 (the shared IMDCT's, module/codecs/dsp). Not: floor 0 (no encoder has used it since 2002), other
 * block sizes, more than two channels: vorbis_init says VORBIS_EUNSUP.
 *
 * Integer only, no division in the per-packet paths, no libc, no malloc: the setup goes into memory the caller
 * gives (two arenas: `fast` for the small structures read in every packet, `slow` for the Huffman tables and
 * vector tables; on the ARM both can be the same block, on Warp0 fast = its local RAM, slow = SDRAM), so the same C
 * runs on the ARM926 and on Warp0. Every index into the setup is checked when the setup is read, every one that
 * depends on a packet when the packet is read: a corrupt stream must not make Warp0 touch memory outside its
 * buffers (a wild access there can hang the bus). */
#ifndef VORBISDEC_H
#define VORBISDEC_H
#include <stdint.h>

#define VORBIS_MAX_CH    2
#define VORBIS_MAX_BLOCK 2048
#define VORBIS_MAX_OUT   (VORBIS_MAX_BLOCK / 2) /* samples per channel one packet can return */

struct vorbis_info {
    uint32_t rate;
    uint8_t channels, mode_bits, modes, pad;
    uint16_t blocksize[2];
    int32_t bitrate_nominal;
};

/* The decoder. VORBIS_EXT_BUFFERS: the overlap and the transition-frame buffer live outside the struct (set
 * `overlap` and `ybuf` after vorbis_init; vorbis_restart clears the overlap), so that the struct itself, the hot
 * working set, fits a small fast RAM (Warp0's local RAM). */
struct vorbis_dec {
    struct vorbis_info info;
    struct vorbis_setup *setup; /* in the fast arena */
    int rp;                     /* binary point of the residue vectors (chosen from the stream's books) */
    int prev_w;                 /* the previous block's size flag, -1 before the first block */
#ifdef VORBIS_EXT_BUFFERS
    int32_t (*overlap)[VORBIS_MAX_BLOCK / 2];
    int32_t *ybuf; /* VORBIS_MAX_BLOCK */
#else
    int32_t overlap[VORBIS_MAX_CH][VORBIS_MAX_BLOCK / 2]; /* the previous block's second half, PCM Q9, unwindowed */
    int32_t ybuf[VORBIS_MAX_BLOCK];                       /* IMDCT output of blocks next to a block of the other size */
#endif
    /* per-packet scratch */
    int32_t spec[VORBIS_MAX_CH][VORBIS_MAX_BLOCK / 2]; /* residue, then spectrum */
    int32_t z[VORBIS_MAX_BLOCK / 2];                   /* IMDCT scratch */
    int16_t fit[VORBIS_MAX_CH][66];                    /* floor 1 posts */
    uint8_t cls[VORBIS_MAX_BLOCK + 128];               /* residue partition classes */
};

/* sizeof(struct vorbis_dec), for callers that allocate it without this header (the Rust binding) */
unsigned vorbis_state_size(void);

/* the stream's format, as vorbis_init read it from the identification header (for rust/) */
const struct vorbis_info *vorbis_stream_info(const struct vorbis_dec *d);

/* Read the identification and setup headers (packets 1 and 5; the comment header is not needed). The setup goes
 * into the arenas (4-byte aligned; slow may equal fast, then pass the same pointer and length twice: it is used
 * once). 0, or a negative VORBIS_E*. */
int vorbis_init(struct vorbis_dec *d, const uint8_t *ident, unsigned ilen, const uint8_t *setup, unsigned slen,
                void *fast, unsigned fast_len, void *slow, unsigned slow_len);

/* Decode one audio packet into interleaved samples. Returns the samples per channel written (0 for the first
 * packet after init or a restart: it only primes the overlap), or a negative VORBIS_E*: the packet is dropped
 * and the state is as before it. */
int vorbis_decode(struct vorbis_dec *d, const uint8_t *pkt, unsigned len, int16_t *pcm);

/* Forget the overlap (after a seek): the next packet primes it again. */
void vorbis_restart(struct vorbis_dec *d);

/* Bytes of arena the last vorbis_init used: fast, slow (both, if they are the same block) */
void vorbis_arena_used(const struct vorbis_dec *d, unsigned *fast, unsigned *slow);

#define VORBIS_EBITS     -1 /* a header ended early */
#define VORBIS_EUNSUP    -2 /* valid Vorbis this decoder doesn't do (floor 0, block sizes, > 2 channels, ...) */
#define VORBIS_EDATA     -3 /* malformed: out-of-range values, a bad codebook */
#define VORBIS_EMEM      -4 /* the arena is too small */
#define VORBIS_ENOTAUDIO -5 /* not an audio packet (a header) */

#endif
