#include <gpio.h>
#include <delay.h>
#include "zd1_board.h"
#include "i2c.h"
#include "dsp_data.h"
#include "dsp.h"

static bool running;
static unsigned vol_idx = DSP_VOL_MUTE;
static dsp_input_t input = DSP_IN_NONE;

bool dsp_running(void) { return running; }
unsigned dsp_volume_index(void) { return vol_idx; }
dsp_input_t dsp_input(void) { return input; }

static int wr(uint16_t sub, const void *d, unsigned n) { return i2c_write(I2C_DSP, sub, 2, d, n); }
static int wr32(uint16_t sub, uint32_t v) {
    uint8_t b[4] = {v >> 24, v >> 16, v >> 8, v};
    return wr(sub, b, 4);
}

/* Power-up and download, stock FUN_80020980: PB03 high, 10 ms, PB02 high, 100 ms, then the 24 records
 * (core control, program RAM 0x2000, config 0xE000.., param RAM 0x0000, start: E228=1 E220=1 E221=0 E221=5). */
int dsp_init(void) {
    int rc;
    running = false;
    gpio_set_pin_high(PIN_DSP_PWR);
    delay_ms(10);
    gpio_set_pin_high(PIN_DSP_RST);
    delay_ms(100);
    for (unsigned i = 0; i < DSP_DOWNLOAD_STEPS; i++) {
        if ((rc = wr(dsp_download[i].subaddr, dsp_download[i].data, dsp_download[i].len)) != 0)
            return rc;
    }
    running = true;
    vol_idx = DSP_VOL_MUTE;
    return dsp_set_volume_index(DSP_VOL_MUTE); /* start muted; the source manager raises it */
}

/* Safeload (FUN_8001fab0): 0x0001 <- value, 0x0006 <- param-1, 0x0007 <- word count (triggers). */
static int dsp_safeload(uint16_t param, uint32_t value) {
    int rc;
    if (!running)
        return -1;
    if ((rc = wr32(0x0001, value)) || (rc = wr32(0x0006, param - 1u)) || (rc = wr32(0x0007, 1)))
        return rc;
    return 0;
}

/* Master volume (FUN_8001fbc8): coefficient from the stock table, safeload to param 0x4B. */
int dsp_set_volume_index(unsigned idx) {
    if (idx > DSP_VOL_MUTE)
        idx = DSP_VOL_MUTE;
    int rc = dsp_safeload(0x004B, dsp_volume_coef[idx]);
    if (rc == 0)
        vol_idx = idx;
    return rc;
}

/* Bass biquad (FUN_8001fc74): 5 words into 0x0001..0x0005, then 0x0006 <- 0x3E, 0x0007 <- 5. */
int dsp_set_bass(unsigned preset) {
    int rc;
    if (!running)
        return -1;
    if (preset >= DSP_BASS_PRESETS)
        preset = DSP_BASS_PRESETS - 1;
    for (unsigned i = 0; i < 5; i++)
        if ((rc = wr32(0x0001 + i, dsp_bass_coef[preset][i])))
            return rc;
    if ((rc = wr32(0x0006, 0x3E)) || (rc = wr32(0x0007, 5)))
        return rc;
    return 0;
}

/* Input routing (FUN_800202bc). Param 0x14 = input selector; E080/E002 = serial port routing for USB vs module.
 * Stock also applies source-specific gain records (0x147.., 0x18E/0x18F) and, for AUX, codec reg 0x0E; those
 * follow in the source manager. 20 ms settle like stock. */
int dsp_set_input(dsp_input_t in) {
    static const uint8_t sel[] = {[DSP_IN_USB] = 0, [DSP_IN_MODULE] = 0, [DSP_IN_AUX] = 2, [DSP_IN_OPTICAL] = 1};
    int rc;
    if (!running)
        return -1;
    if (in < DSP_IN_USB || in > DSP_IN_OPTICAL)
        return -2;
    if ((rc = wr32(0x0014, sel[in])))
        return rc;
    delay_ms(20);
    if (in == DSP_IN_USB) {
        static const uint8_t e080[] = {0x00, 0x00}, e002[] = {0x08, 0x00};
        if ((rc = wr(0xE080, e080, 2)) || (rc = wr(0xE002, e002, 2)))
            return rc;
    } else if (in == DSP_IN_MODULE) {
        static const uint8_t e080[] = {0x00, 0x02}, e002[] = {0x09, 0x08};
        if ((rc = wr(0xE080, e080, 2)) || (rc = wr(0xE002, e002, 2)))
            return rc;
    }
    input = in;
    return 0;
}

/* The input level meter (param 0x031, stock's): a linear peak, 5.23 (1.0 = 0x800000 = full scale), of the selected
 * input, before the volume stage; a peak follower that falls ~50 dB in 0.4 s after the music stops [HW 2026-10-27].
 * -1: no answer. */
int32_t dsp_level(void) {
    uint8_t b[4];
    if (!running || i2c_read(I2C_DSP, 0x0031, 2, b, 4))
        return -1;
    int32_t v = (int32_t)((uint32_t)b[0] << 24 | (uint32_t)b[1] << 16 | (uint32_t)b[2] << 8 | b[3]);
    return v < 0 ? -v : v;
}
/* S/PDIF receiver lock (FUN_8001f8e8): read 2 bytes at 0xE0C9, bit0 of the second byte. */
int dsp_spdif_locked(void) {
    uint8_t b[2];
    if (!running)
        return 0;
    int rc = i2c_read(I2C_DSP, 0xE0C9, 2, b, 2);
    return rc ? rc : (b[1] & 1);
}
