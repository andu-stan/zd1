#include <gpio.h>
#include "zd1_board.h"
#include "tick.h"
#include "inputs.h"

#define SAMPLE_MS    5
#define STABLE_N     4   /* 20 ms debounce */
#define REPEAT_DELAY 500 /* volume auto-repeat */
#define REPEAT_EVERY 150

typedef struct {
    uint32_t pin;
    bool active_low;
    uint8_t cnt;
    bool raw, stable;
} deb_t;
enum { D_AUX, D_VBUS, D_ONOFF, D_VOLUP, D_VOLDN, D_REAR, D_N };
static deb_t d[D_N] = {
    [D_AUX] = {PIN_AUX_DETECT, true},  [D_VBUS] = {PIN_USB_VBUS, false},  [D_ONOFF] = {PIN_KEY_ONOFF, true},
    [D_VOLUP] = {PIN_KEY_VOLUP, true}, [D_VOLDN] = {PIN_KEY_VOLDN, true}, [D_REAR] = {PIN_KEY_REAR, true},
};
static inputs_t st;
static uint32_t next_sample;
static key_event_t q[8];
static uint8_t qh, qt;
static uint32_t rep_at;
static int rep_key = -1;
static uint32_t onoff_at;
static bool onoff_down; /* ON/OFF press time, held and not yet a hold */

static void push(key_event_t k) {
    uint8_t n = (qt + 1) & 7;
    if (n == qh)
        return;
    q[qt] = k;
    qt = n;
}
void inputs_inject_key(key_event_t k) { push(k); }
key_event_t inputs_key(void) {
    if (qh == qt)
        return KEY_NONE;
    key_event_t k = q[qh];
    qh = (qh + 1) & 7;
    return k;
}
const inputs_t *inputs(void) { return &st; }

void inputs_init(void) {
    for (int i = 0; i < D_N; i++) {
        gpio_configure_pin(d[i].pin, GPIO_DIR_INPUT);
        bool v = gpio_get_pin_value(d[i].pin) ^ d[i].active_low;
        d[i].raw = d[i].stable = v;
        d[i].cnt = STABLE_N;
    }
    st = (inputs_t){d[D_AUX].stable, d[D_VBUS].stable};
}

void inputs_poll(uint32_t now) {
    static const key_event_t keyof[D_N] = {
        [D_ONOFF] = KEY_ONOFF, [D_VOLUP] = KEY_VOLUP, [D_VOLDN] = KEY_VOLDN, [D_REAR] = KEY_REAR};
    if (!tick_after(now, next_sample))
        return;
    next_sample = now + SAMPLE_MS;
    for (int i = 0; i < D_N; i++) {
        bool v = gpio_get_pin_value(d[i].pin) ^ d[i].active_low;
        if (v != d[i].raw) {
            d[i].raw = v;
            d[i].cnt = 0;
            continue;
        }
        if (d[i].cnt < STABLE_N && ++d[i].cnt == STABLE_N && v != d[i].stable) {
            d[i].stable = v;
            if (keyof[i]) {
                if (v) {
                    push(keyof[i]);
                    if (i == D_VOLUP || i == D_VOLDN) {
                        rep_key = i;
                        rep_at = now + REPEAT_DELAY;
                    }
                    if (i == D_ONOFF) {
                        onoff_down = true;
                        onoff_at = now;
                    }
                } else {
                    if (rep_key == i)
                        rep_key = -1;
                    if (i == D_ONOFF && onoff_down) {
                        onoff_down = false;
                        push(KEY_ONOFF_SHORT);
                    }
                }
            }
        }
    }
    if (onoff_down && tick_after(now, onoff_at + ONOFF_HOLD_MS)) {
        onoff_down = false;
        push(KEY_ONOFF_HOLD);
    }
    if (rep_key >= 0 && d[rep_key].stable && tick_after(now, rep_at)) {
        push(keyof[rep_key]); /* auto-repeat */
        rep_at = now + REPEAT_EVERY;
    }
    st = (inputs_t){d[D_AUX].stable, d[D_VBUS].stable};
}
