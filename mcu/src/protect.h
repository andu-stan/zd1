/* Protections (stock FUN_800235fc, docs/hardware/zd1-main-board.md "Protections"), armed only while the amp supply is on (their behaviour
 * with the power stage off is unknown). Trip -> source_error(code): amp off + mute + red blink, until ON/OFF.
 *   1: ADC5 (PA26, temperature) < 0x1FF for 9 samples at 500 ms (a conversion that times out counts as under)
 *   2: PB18 (amp fault/clip) low > 79 % of the time over 3 s
 *   3: ADC4 (PA25, a second temperature) < 0x1BC for 9 samples at 500 ms (likewise)
 *   4: PB04 (fault) low (3 consecutive samples)
 * ADC set up like stock (MR 0x0F1F1E00) so the thresholds keep their meaning. ADC6 (PA27, the amp supply rail) is
 * read for info.
 * Ours on top: a heat soft limit (ADC5 under 0x280 or ADC4 under 0x248 for 3 s, ~66 °C, caps the volume at step 50
 * until both are back over 0x2A0 / 0x264) and both in the log every 10 minutes the amp has been on. */
#ifndef PROTECT_H
#define PROTECT_H
#include <stdint.h>
#include <stdbool.h>

void protect_init(void);
void protect_poll(uint32_t now);
/* the last samples (ADC4, ADC5, ADC6; -1: none yet) and the trip counters (consecutive samples under a limit) */
void protect_state(int adc[3], uint8_t *n_temp, uint8_t *n_adc4, uint8_t *n_fault);
uint8_t protect_heat_cap(void); /* the highest user step the heat allows (VOL_MAX: no cap) */

#endif
