/* Class-D power stage: PB09 = supply enable, PB16 = unmute. Stock: enable, ~2 s soft start, then unmute while
 * ramping the DSP volume; off = mute first, then the supply. Ours can hold the supply on (muted) for a while after
 * it is let go, so a quick standby -> run or idle -> sound doesn't cycle it (each cycle stresses its caps). */
#ifndef AMP_H
#define AMP_H
#include <compiler.h>

void amp_off(void);                 /* mute + supply off now (always safe: errors, the interlock) */
void amp_release(uint32_t hold_ms); /* mute now, supply off after hold_ms unless amp_supply_on comes first */
void amp_supply_on(void); /* PB09 high; unmute is allowed 2 s later (the soft start; at once if still held on) */
bool amp_unmute(void);    /* PB16 high; refused (false) until the soft start has elapsed */
void amp_mute(void);
void amp_poll(uint32_t now); /* the held supply's end */
bool amp_is_on(void);
bool amp_is_unmuted(void);
uint32_t amp_on_s(void);  /* the supply's on-time since boot, seconds */
uint32_t amp_run_s(void); /* since it last came on (0: off) */

#endif
