/* Cirrus CS2000-CP clock multiplier: makes the audio MCLK (256 fs) from the 12 MHz GCLK2 reference (PB30).
 * Bit-banged I2C on PB13 (SCL) / PB14 (SDA), 8-bit address 0x9C (PB13/PB14 are not TWI pins). */
#ifndef CS2000_H
#define CS2000_H
#include <compiler.h>

int cs2000_init(void);                  /* stock sequence (44.1 kHz ratio). Returns number of NAKed bytes */
int cs2000_set_rate(uint32_t fs);       /* 32000 / 44100 / 48000 */
uint32_t cs2000_ratio_for(uint32_t fs); /* nominal 12.20 ratio for that rate */
int cs2000_set_ratio(uint32_t ratio);   /* raw ratio (rate trim); returns NAK count */
void cs2000_refclk_on(void);            /* OSC1 + GCLK0 + GCLK2 exactly like stock (GCLK2 = OSC1/4 on PB30) */

#endif
