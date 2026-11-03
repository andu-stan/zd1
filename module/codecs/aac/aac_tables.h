/* Tables for the AAC-LC decoder (aacdec.c), generated into aac_tables.c by tools/gentables.py. */
#ifndef AAC_TABLES_H
#define AAC_TABLES_H
#include <stdint.h>

/* AAC_FAST: placement of the tables every frame reads all of (the windows; the IMDCT's are module/codecs/dsp's, DSP_FAST);
 * empty by default, a section attribute where they go to fast RAM (Warp0) */
#ifndef AAC_FAST
#define AAC_FAST
#endif

/* Huffman lookup (8-bit root): leaf = value << 16 | length; link = 0x8000 | offset << 16 | subtable bits */
extern const uint32_t aac_huff_sf[];
extern const uint32_t *const aac_huff_spec[12]; /* books 1..11; quads pack 4 x 4-bit, pairs 2 x 8-bit */

extern const uint16_t *const aac_swb_long[13], *const aac_swb_short[13];
extern const uint8_t aac_num_swb_long[13], aac_num_swb_short[13];
extern const uint8_t aac_tns_max_bands_long[13], aac_tns_max_bands_short[13];

extern const uint32_t aac_pow43[1025];       /* x^(4/3), Q17 */
extern const uint32_t aac_pow2_frac[4];      /* 2^(f/4), Q30 */
extern const int32_t *const aac_tns_coef[4]; /* [coef_compress * 2 + (coef_res bits - 3)], Q31 */

extern const int32_t aac_win_long_sine[1024], aac_win_long_kbd[1024]; /* rising halves, Q31 */
extern const int32_t aac_win_short_sine[128], aac_win_short_kbd[128];

#endif
