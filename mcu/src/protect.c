#include <sysclk.h>
#include <gpio.h>
#include <adc.h>
#include "zd1_board.h"
#include "log.h"
#include "tick.h"
#include "amp.h"
#include "source.h"
#include "protect.h"
#include "settings.h"

#define SAMPLE_MS   500
#define TEMP_MIN    0x1FF
#define TEMP_N      9
#define ADC4_MIN    0x1BC /* ADC4: a second thermistor, checked by stock like ADC5 (FUN_8001c6a0) */
#define FAULT_N     3
#define CLIP_WIN_MS 3000
#define CLIP_PCT    79
/* Heat (ours): ADC4 and ADC5 fall as the board warms (0x37F / 0x398 cold at 26 °C, 0x2D9 / 0x338 playing
 * [HW 2026-10-27]); stock trips at 0x1BC / 0x1FF, both ~81 °C read as NTCs (docs/hardware/zd1-main-board.md). Either
 * one under its soft limit caps the volume until both have cooled past their clear value: 66 / 63 °C on each, a
 * first guess between the hottest seen (~51 °C on ADC5) and the trip. ADC4 runs 7-9 °C hotter. */
#define HEAT_SOFT   0x280 /* ADC5 */
#define HEAT_CLEAR  0x2A0
#define HEAT4_SOFT  0x248 /* ADC4 */
#define HEAT4_CLEAR 0x264
#define HEAT_N      6   /* samples in a row (3 s) */
#define HEAT_CAP    50  /* user step: -11 dB */
#define HEAT_LOG_S  600 /* ADC4 and ADC5 in the log every 10 min the amp has been on */

static uint32_t next_sample, clip_t0, clip_n, clip_low;
static uint8_t n_temp, n_adc4, n_fault, n_heat, heat_cap = VOL_MAX;
static uint32_t heat_logged;
static int adc4 = -1, adc5 = -1, adc6 = -1; /* the last samples (-1: none, or the conversion timed out) */

void protect_init(void) {
    static const gpio_map_t pins = {{AVR32_ADC_AD_4_PIN, AVR32_ADC_AD_4_FUNCTION},
                                    {AVR32_ADC_AD_5_PIN, AVR32_ADC_AD_5_FUNCTION},
                                    {AVR32_ADC_AD_6_PIN, AVR32_ADC_AD_6_FUNCTION}};
    gpio_enable_module(pins, 3);
    sysclk_enable_pba_module(SYSCLK_ADC);
    adc_configure(&AVR32_ADC); /* SHTIM 0xF, STARTUP 0x1F (= stock) */
    AVR32_ADC.mr =
        (AVR32_ADC.mr & ~AVR32_ADC_MR_PRESCAL_MASK) | (0x1Eu << AVR32_ADC_MR_PRESCAL_OFFSET); /* stock 0x0F1F1E00 */
}

static int protect_adc(unsigned ch) { /* one conversion, -1 on timeout */
    adc_enable(&AVR32_ADC, ch);
    adc_start(&AVR32_ADC);
    for (uint32_t t = 0; !adc_check_eoc(&AVR32_ADC, ch); t++)
        if (t > 100000) {
            adc_disable(&AVR32_ADC, ch);
            return -1;
        }
    int v = (int)adc_get_value(&AVR32_ADC, ch); /* EOC set: no wait */
    adc_disable(&AVR32_ADC, ch);
    return v;
}

void protect_poll(uint32_t now) {
    bool armed = amp_is_on() && source_state() != SYS_ERROR;
    /* clip/fault duty on PB18, sampled every main-loop pass that is >= 5 ms apart */
    static uint32_t next_clip;
    if (armed && tick_after(now, next_clip)) {
        next_clip = now + 5;
        if (!clip_n)
            clip_t0 = now;
        clip_n++;
        clip_low += !gpio_get_pin_value(PIN_AMP_FAULT);
        if (tick_after(now, clip_t0 + CLIP_WIN_MS)) {
            if (clip_low * 100 > clip_n * CLIP_PCT)
                source_error(2);
            clip_n = clip_low = 0;
        }
    }
    if (!armed) {
        clip_n = clip_low = 0;
        n_temp = n_adc4 = n_fault = 0;
    }
    if (!tick_after(now, next_sample))
        return;
    next_sample = now + SAMPLE_MS;
    adc4 = protect_adc(4);
    adc5 = protect_adc(5);
    adc6 = protect_adc(6);
    if (adc4 >= 0 && adc5 >= 0) {
        bool capped = heat_cap < VOL_MAX;
        bool hot = capped ? adc5 < HEAT_CLEAR || adc4 < HEAT4_CLEAR : adc5 < HEAT_SOFT || adc4 < HEAT4_SOFT;
        n_heat = hot == capped ? 0 : n_heat + 1; /* samples in a row that say otherwise */
        if (n_heat >= HEAT_N) {
            n_heat = 0;
            heat_cap = capped ? VOL_MAX : HEAT_CAP;
            log_puts(capped ? "[protect] cooled (ADC5 " : "[protect] hot (ADC5 ");
            log_hexn((uint32_t)adc5, 3);
            log_puts(", ADC4 ");
            log_hexn((uint32_t)adc4, 3);
            log_puts(capped ? "): volume cap lifted\n" : "): volume capped\n");
        }
    }
    uint32_t run = amp_run_s();
    if (!run)
        heat_logged = 0;
    else if (run >= heat_logged + HEAT_LOG_S) {
        heat_logged = run - run % HEAT_LOG_S;
        log_puts("[amp] on ");
        log_dec(run / 60);
        log_puts(" min, ADC5 ");
        log_hexn((uint32_t)adc5, 3);
        log_puts(", ADC4 ");
        log_hexn((uint32_t)adc4, 3);
        log_putc('\n');
    }
    if (!armed)
        return;
    /* a conversion that times out (-1) counts as under the limit: a sensor that can't be read doesn't say it's cool */
    n_temp = adc5 < TEMP_MIN ? n_temp + 1 : 0;
    n_adc4 = adc4 < ADC4_MIN ? n_adc4 + 1 : 0;
    n_fault = !gpio_get_pin_value(PIN_FAULT) ? n_fault + 1 : 0;
    if (n_temp >= TEMP_N || n_adc4 >= TEMP_N) {
        if (adc5 < 0 || adc4 < 0)
            log_puts("[protect] the ADC's conversions time out\n");
        source_error(n_temp >= TEMP_N ? 1 : 3);
    } else if (n_fault >= FAULT_N)
        source_error(4);
}

uint8_t protect_heat_cap(void) { return heat_cap; }

void protect_state(int adc[3], uint8_t *temp, uint8_t *temp2, uint8_t *fault) {
    adc[0] = adc4;
    adc[1] = adc5;
    adc[2] = adc6;
    *temp = n_temp;
    *temp2 = n_adc4;
    *fault = n_fault;
}
