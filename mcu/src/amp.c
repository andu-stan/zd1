#include <gpio.h>
#include "zd1_board.h"
#include "tick.h"
#include "log.h"
#include "amp.h"

#define SOFTSTART_MS 2000 /* stock: the supply on, then unmute ~2 s later */

/* the soft start on the ms clock, latched once over: a cycle-counter timeout (ASF's) wraps every 89.5 s and would
 * refuse an unmute for up to 2 s on a supply that has been on for minutes */
static bool on, held, soft_done;
static uint32_t t_on, t_hold_end, on_ms; /* on_ms: the on-periods that ended */

bool amp_is_on(void) { return on; }
bool amp_is_unmuted(void) { return gpio_get_gpio_pin_output_value(PIN_AMP_UNMUTE); }
uint32_t amp_run_s(void) { return on ? (tick_ms() - t_on) / 1000 : 0; }
uint32_t amp_on_s(void) { return (on_ms + (on ? tick_ms() - t_on : 0)) / 1000; }

void amp_mute(void) { gpio_set_pin_low(PIN_AMP_UNMUTE); }
void amp_off(void) {
    amp_mute();
    gpio_set_pin_low(PIN_AMP_EN);
    if (on) {
        uint32_t ms = tick_ms() - t_on;
        on_ms += ms;
        log_puts("[amp] supply off after ");
        log_dec(ms / 1000);
        log_puts(" s\n");
    }
    on = held = false;
}
void amp_release(uint32_t hold_ms) {
    amp_mute();
    if (!on)
        return;
    if (!hold_ms) {
        amp_off();
        return;
    }
    if (!held) {
        held = true;
        t_hold_end = tick_ms() + hold_ms;
    }
}
void amp_supply_on(void) {
    held = false; /* still on (held): no new soft start */
    if (on)
        return;
    amp_mute();
    gpio_set_pin_high(PIN_AMP_EN);
    on = true;
    soft_done = false;
    t_on = tick_ms();
    log_puts("[amp] supply on\n");
}
bool amp_unmute(void) {
    if (!on || held)
        return false;
    if (!soft_done && !tick_after(tick_ms(), t_on + SOFTSTART_MS))
        return false;
    soft_done = true;
    gpio_set_pin_high(PIN_AMP_UNMUTE);
    return true;
}
void amp_poll(uint32_t now) {
    if (on && !soft_done && tick_after(now, t_on + SOFTSTART_MS))
        soft_done = true;
    if (held && tick_after(now, t_hold_end))
        amp_off();
}
