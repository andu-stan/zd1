/* ADAU1445 DSP (I2C 0x38, 2-byte sub-address): power-up + stock download, safeload parameters, volume, bass,
 * input routing, S/PDIF lock. Behaviour reproduces stock 2.04.05 (docs/mcu/stock-firmware.md,
 * "DSP"; re/mcu/extracted/dsp/). */
#ifndef DSP_H
#define DSP_H
#include <compiler.h>

/* input routing modes = stock FUN_800202bc */
typedef enum {
    DSP_IN_NONE = 0,
    DSP_IN_USB = 1,     /* SSC I2S from the MCU (USB audio, iPod digital) */
    DSP_IN_MODULE = 2,  /* network module I2S (module drives the clocks) */
    DSP_IN_AUX = 3,     /* AUX analog via the codec ADC */
    DSP_IN_OPTICAL = 4, /* AUX optical (S/PDIF receiver in the DSP) */
} dsp_input_t;

#define DSP_VOL_MUTE 71 /* volume index: 0 = +10 dB .. 70 = -60 dB, 71 = mute */

int dsp_init(void); /* power (PB03), reset release (PB02), 24-step download, start. 0 = OK */
bool dsp_running(void);
int dsp_set_volume_index(unsigned idx); /* 0..DSP_VOL_MUTE */
int dsp_set_bass(unsigned preset);      /* 0..DSP_BASS_PRESETS - 1 */
int dsp_set_input(dsp_input_t in);
int dsp_spdif_locked(void); /* 1 locked, 0 not, <0 error (reads 0xE0C9) */
/* the input meter (param 0x031): linear peak of the selected input, 5.23 (0x800000 = full scale); -1: no answer */
int32_t dsp_level(void);
unsigned dsp_volume_index(void);
dsp_input_t dsp_input(void);

#endif
