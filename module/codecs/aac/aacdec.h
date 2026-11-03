/* AAC-LC decoder, fixed point (aacdec.c): raw access units of one or two channels in, 16-bit PCM out.
 *
 * What AirPlay 2 sends for music, and nothing more: the low-complexity profile (no SBR/PS, no main or LTP
 * prediction, no coupling), sampling rates 8-96 kHz, mono or stereo. The tools LC streams use are all here:
 * mid/side and intensity stereo, noise substitution, pulses, temporal noise shaping, long/start/short/stop windows
 * with sine or KBD shapes. Integer only (no float, no division in the per-sample paths), so it runs on the ARM926
 * and on Warp0. */
#ifndef AACDEC_H
#define AACDEC_H
#include <stdint.h>

#define AAC_FRAME 1024 /* samples per channel per access unit */

struct aac_ics { /* one channel's side info for the current frame */
    /* ics_info, up to and including swb: copied by words for a pair's common window (aacdec.c, ICS_INFO_WORDS) */
    uint8_t group_len[8];
    uint8_t win_seq, win_shape, max_sfb, num_windows, num_groups, num_swb, tns_max, pad;
    const uint16_t *swb;
    uint8_t cb[8][64]; /* per window group and band: the codebook (0 zero, 1-11, 13 noise, 14/15 intensity) */
    int16_t sf[8][64]; /* scale factor / noise energy / intensity position, by codebook */
    uint8_t pulse, pulse_n, pulse_start, pulse_off[4], pulse_amp[4];
    uint8_t tns;
    struct {
        uint8_t n_filt;
        struct {
            uint8_t length, order, dir;
            int32_t coef[12];
        } f[3]; /* reflection coefficients, Q31 */
    } tns_w[8];
};

/* AAC_EXT_BUFFERS: the overlap, the generic synthesis buffer and the noise seeds live outside the struct (the
 * caller points them at memory of its choice after aac_init and clears the overlap), so that the struct itself, the
 * hot working set, fits a small fast RAM (Warp0's local RAM) */
struct aac_dec {
    uint8_t sf_index, channels;
    uint32_t seed; /* noise substitution */
#ifdef AAC_EXT_BUFFERS
    int32_t (*overlap)[AAC_FRAME];
    int32_t *buf;
    uint32_t (*noise_seed)[64];
#else
    int32_t overlap[2][AAC_FRAME]; /* the previous frame's second half, windowed, PCM Q8 */
#endif
    uint8_t prev_shape[2];
    /* per-frame scratch */
    struct aac_ics ics[2];
    uint8_t ms[8][64], ms_present; /* ms first: word aligned (zero() clears it by words) */
#ifndef AAC_EXT_BUFFERS
    uint32_t noise_seed[8][64];
#endif
    int16_t q[AAC_FRAME] __attribute__((aligned(4))); /* quantized values (zero() clears them by words) */
    int32_t spec[2][AAC_FRAME];                       /* dequantized spectra, window order, Q2 */
#ifndef AAC_EXT_BUFFERS
    int32_t buf[2 * AAC_FRAME]; /* IMDCT output / windowed frame (start, stop and short blocks) */
#endif
    int32_t fft[2 * 512];
};

/* sizeof(struct aac_dec), for callers that allocate it without this header (the Rust binding) */
unsigned aac_state_size(void);

/* sample_rate: 8000-96000 (one of the standard rates); channels 1 or 2. 0, or -1 for an unsupported format. */
int aac_init(struct aac_dec *d, unsigned sample_rate, unsigned channels);

/* Decode one raw access unit into 1024 * channels interleaved samples. Returns the number of samples written, or a
 * negative error (AAC_E*): the frame is dropped, the decoder stays usable. */
int aac_decode(struct aac_dec *d, const uint8_t *in, unsigned len, int16_t *pcm);

#define AAC_EBITS  -1 /* ran past the end of the frame */
#define AAC_EUNSUP -2 /* a feature outside AAC-LC (prediction, gain control, coupling, PCE, ...) */
#define AAC_EDATA  -3 /* malformed: out-of-range values */

#endif
