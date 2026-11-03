/* AD193x codec (I2C 0x04, 1-byte register address): ADC for AUX analog, MCLK = 256 fs. */
#ifndef CODEC_H
#define CODEC_H
#include <compiler.h>

int codec_init(void);          /* stock sequence; 0 = OK, -100 = PLL never locked, else I2C error */
int codec_aux_mode(void);      /* reg 0x0E = 0x72 (stock, AUX analog) */
int codec_dac_mute(bool mute); /* reg 4 bit 0 = DAC master mute; init leaves it set, unmute after the amp (stock) */
unsigned codec_lock_ms(void);  /* how long the last init took to see PLL lock */

#endif
