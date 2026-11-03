/* Tables for the Vorbis decoder (vorbisdec.c), generated into vorbis_tables.c by tools/gentables.py. */
#ifndef VORBIS_TABLES_H
#define VORBIS_TABLES_H
#include <stdint.h>

/* VORBIS_FAST: placement of the tables every packet reads; empty by default, a section attribute where they go to
 * fast RAM (Warp0) */
#ifndef VORBIS_FAST
#define VORBIS_FAST
#endif

extern const int32_t vorbis_floor1_db[256];                      /* floor 1 inverse dB table, Q31 */
extern const int32_t vorbis_win_128[128], vorbis_win_1024[1024]; /* rising window slopes, Q31 */

#endif
