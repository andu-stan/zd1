/* FLAC frame decoder (fixed point, freestanding): what a Snapcast server sends with codec=flac. README.md.
 *
 * Whole frames in (the stream's STREAMINFO given once), interleaved int32 out with each sample left-justified (the
 * top bits, as S32 audio). No libc, no division, no allocation, every read bounded by the input: the same C runs on
 * the ARM926 and on Warp0 (module/warp0, the FLAC image), where an access outside its memory hangs the bus. */
#ifndef FLACDEC_H
#define FLACDEC_H
#include <stdint.h>

/* Limits, per build: the defaults are Warp0's (its 48 KB of local RAM); the ARM's build (rust/build.rs) takes what
 * FLAC allows, 8 channels and 65535-sample blocks. The working memory is one channel's block (FLAC_MAX_BLOCK x 4). */
#ifndef FLAC_MAX_CH
#define FLAC_MAX_CH 2 /* up to 8 (FLAC's limit); the stereo modes only with 2 */
#endif
#ifndef FLAC_MAX_BLOCK
#define FLAC_MAX_BLOCK 4608 /* the streamable subset's limit at <= 48 kHz (libFLAC's levels: 1152 or 4096) */
#endif
#define FLAC_MAX_BPS 24

/* results (negative) */
#define FLAC_EINFO     -1  /* STREAMINFO short or invalid */
#define FLAC_EUNSUP    -2  /* more channels, more bits or longer blocks than above */
#define FLAC_ESYNC     -3  /* no frame sync code where a frame should start */
#define FLAC_EHEADER   -4  /* a reserved value in the frame header */
#define FLAC_ECRC8     -5  /* the frame header's CRC-8 */
#define FLAC_ESUBFRAME -6  /* a reserved subframe type, or an LPC precision / shift that isn't allowed */
#define FLAC_ERESIDUAL -7  /* a reserved residual coding, or a partition order the block can't hold */
#define FLAC_EOVERRUN  -8  /* the frame runs past the input */
#define FLAC_ECRC16    -9  /* the frame's CRC-16 */
#define FLAC_EFORMAT   -10 /* the frame's channels or bits differ from STREAMINFO's */
#define FLAC_EOUTPUT   -11 /* no room in the output for the next frame (nothing of it written) */

#ifdef FLAC_STATS
enum {
    FLAC_S_CONSTANT,
    FLAC_S_VERBATIM,
    FLAC_S_FIXED,
    FLAC_S_LPC,
    FLAC_S_LPC_WIDE,
    FLAC_S_WASTED,
    FLAC_S_ESCAPE,
    FLAC_S_RICE2,
    FLAC_S_LEFT_SIDE,
    FLAC_S_RIGHT_SIDE,
    FLAC_S_MID_SIDE,
    FLAC_S_INDEPENDENT,
    FLAC_NSTATS
};
extern unsigned long flac_stats[FLAC_NSTATS];
#endif

struct flac_info {
    uint32_t rate, channels, bits, min_block, max_block;
};

struct flac_dec {
    struct flac_info info;
    int32_t buf[FLAC_MAX_BLOCK]; /* one channel of a frame (the other is combined in the output) */
    uint32_t frames;             /* frames decoded */
};

/* The 34-byte STREAMINFO block body (after the 4-byte metadata block header). 0, FLAC_EINFO or FLAC_EUNSUP. */
int flac_init(struct flac_dec *d, const uint8_t *streaminfo, uint32_t len);

/* sizeof(struct flac_dec) in this build (for callers that allocate it: rust/) */
uint32_t flac_state_size(void);

/* the stream's format, as flac_init read it from STREAMINFO (for callers without this header: rust/) */
const struct flac_info *flac_stream_info(const struct flac_dec *d);

/* Decode whole frames from `in` (word aligned; the bytes up to len rounded up to 4 readable) into `out`, at most
 * `out_cap` samples (frames x channels). *used = bytes of input taken (the frames decoded), *samples = samples
 * written. Stops at the end of the input, or before a frame that doesn't fit (FLAC_EOUTPUT if not even the first
 * one does: nothing of it is written). A frame that doesn't decode: its error, its part of `out` scribbled on; the
 * frames before it stay in `out` (*samples). */
int flac_decode(struct flac_dec *d, const uint8_t *in, uint32_t len, int32_t *out, uint32_t out_cap, uint32_t *used,
                uint32_t *samples);

/* The same from byte `start` of `in` (which stays word aligned): the frames after the ones a full output stopped at,
 * without copying the rest down. *used counts from the start of `in`. */
int flac_decode_from(struct flac_dec *d, const uint8_t *in, uint32_t len, uint32_t start, int32_t *out,
                     uint32_t out_cap, uint32_t *used, uint32_t *samples);

#endif
