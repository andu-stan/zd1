/* ADAU1445 data from stock 2.04.05: the 24-step download (program RAM, config, param RAM, start) and runtime tables.
 * Generated into dsp_data.c by tools/gen_dsp_data.py. */
#ifndef DSP_DATA_H
#define DSP_DATA_H
#include <stdint.h>

typedef struct {
    uint16_t subaddr;
    uint16_t len;
    const uint8_t *data;
} dsp_rec_t;
#define DSP_DOWNLOAD_STEPS 24
extern const dsp_rec_t dsp_download[DSP_DOWNLOAD_STEPS];
extern const uint32_t dsp_volume_coef[72];
#define DSP_BASS_PRESETS 5
extern const uint32_t dsp_bass_coef[DSP_BASS_PRESETS][5];

#endif
