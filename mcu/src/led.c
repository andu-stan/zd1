#include <gpio.h>
#include <pwm.h>
#include "tick.h"
#include "led.h"
#include "source.h"
#include "zd1link.h"
#include "settings.h"

#define PERIOD   400
#define FRAME_MS 10   /* a new colour every 10 ms (the PWM takes it at its next period, 8.5 ms) */
#define FADE_MS  450  /* crossfade between looks */
#define VOL_MS   1200 /* volume feedback */
#define ACK_MS   900  /* a pick acknowledged: two pulses */

/* CUPD with CMR.CPD = 0 -> new duty at the next period boundary. Async: ASF's sync variant busy-waits for the
 * period end (~8.5 ms per channel), which would stall the cooperative main loop. */
static void duty(unsigned ch, uint16_t v) {
    avr32_pwm_channel_t c;
    c.cmr = 0x20A; /* anonymous union: GCC 4.4 can't designate it in an initializer */
    c.cupd = v > PERIOD ? PERIOD : v;
    pwm_async_update_channel(ch, &c);
}
static void rgb(const uint16_t v[3]) {
    static uint16_t last[3] = {0xFFFF, 0xFFFF, 0xFFFF};
    for (unsigned i = 0; i < 3; i++)
        if (v[i] != last[i]) {
            duty(i + 1, v[i]);
            last[i] = v[i];
        }
}

/* Stock PWM setup (zd1 emulator trace of 2.04.05): MR = 0, CMR = 0x20A (MCK/1024, CPOL = 1), CPRD = 400,
 * channels 1..3 on PB20/PB21/PB22 (function A), ENA = 0x0E. */
void led_init(void) {
    static const gpio_map_t pins = {{AVR32_PWM_1_PIN, AVR32_PWM_1_FUNCTION},
                                    {AVR32_PWM_2_PIN, AVR32_PWM_2_FUNCTION},
                                    {AVR32_PWM_3_PIN, AVR32_PWM_3_FUNCTION}};
    const pwm_opt_t opt = {.diva = 0, .divb = 0, .prea = 0, .preb = 0};
    avr32_pwm_channel_t c;
    c.cmr = 0x20A;
    c.cdty = 0;
    c.cprd = PERIOD;
    pwm_init(&opt);
    for (unsigned ch = 1; ch <= 3; ch++)
        pwm_channel_init(ch, &c);
    gpio_enable_module(pins, 3);
    pwm_start_channels(0x0E);
}

/* ---- what to show ---- */
static led_look_t look = {LOOK_BOOT, SRC_NONE, ZD1_PLAYER_NONE, ACT_READY};
static bool overlay; /* a blink over everything (led_flash) */
static led_flash_t overlay_c;
static uint32_t overlay_until;
static bool vol_on, vol_limit; /* volume feedback */
static uint8_t vol_level;
static uint32_t vol_until;
static bool ack_on; /* a person's pick, acknowledged */
static uint8_t ack_src, ack_player;
static uint32_t ack_t0;

/* ---- drawing ----
 * Colours and levels in perceptual units (0..255); the PWM gets level² (gamma 2), so fades and breathing look even.
 * The palette is fitted to this LED as measured (a camera in Apple Log filming a duty sequence [HW 2026-10-27];
 * mcu/tools/led_model.json): light linear in duty and additive; at full duty red gives ~1/3 of green's light, blue
 * is strong, and equal duty is a bluish white (x 0.235, y 0.204), so anything with some blue drifts to blue-white (a
 * violet looked like the computer's blue, a lavender white; the measured model predicts both, an sRGB one didn't).
 * Chosen by maximising the smallest CIEDE2000 between looks that must be told apart (every colour playing and
 * asleep, asleep against standby, playing against the error; mcu/tools/led_palette.py with that model): 19.7,
 * seen on the unit. Wi-Fi without a named receiver shares AirPlay's white (the speaker's main use); its states differ
 * by animation, and the freed colour is the dock's. */
typedef struct {
    uint8_t r, g, b;
} rgb8_t;
static const rgb8_t RED = {255, 0, 0};
static const rgb8_t flash_color[] = {
    [FLASH_PURPLE] = {170, 0, 255}, [FLASH_GREEN] = {0, 255, 40}, [FLASH_RED] = {255, 0, 0}};
#define WHITE {255, 240, 120} /* little blue: a neutral white on this LED needs duties ~334/400/67 */
static const rgb8_t src_color[SRC_N] = {
    [SRC_NONE] = {255, 0, 0}, [SRC_USB] = {0, 174, 255},  [SRC_AUX] = {255, 221, 0}, /* AUX orange here */
    [SRC_WIFI] = WHITE,       [SRC_IPOD] = {255, 0, 161},                            /* the dock: magenta-pink */
};
static const rgb8_t player_color[] = {
    [ZD1_PLAYER_NONE] = WHITE,
    [ZD1_PLAYER_AIRPLAY] = WHITE,
    [ZD1_PLAYER_SPOTIFY] = {106, 255, 0},
    [ZD1_PLAYER_SNAPCAST] = {0, 255, 128},
    [ZD1_PLAYER_OTHER] = WHITE,
};
#define N_PLAYERS (sizeof player_color / sizeof player_color[0])

static uint16_t out[3], from[3]; /* shown and fade start, perceptual << 8 */
static uint32_t fade_t0, fade_ms, next_frame;
static uint16_t scene; /* what changing starts a crossfade */

static uint32_t ease(uint32_t x) { return x * x * (768 - 2 * x) >> 16; } /* smoothstep, 0..256 -> 0..256 */
/* level going lo -> hi -> lo over per ms */
static uint8_t breathe(uint32_t now, uint32_t per, uint8_t lo, uint8_t hi) {
    uint32_t t = now % per, x = (t < per / 2 ? t : per - t) * 512 / per; /* 0..256 */
    return (uint8_t)(lo + (hi - lo) * ease(x) / 256);
}
static rgb8_t color_of(uint8_t source, uint8_t player) {
    if (source == SRC_WIFI && player < N_PLAYERS)
        return player_color[player];
    return source < SRC_N ? src_color[source] : RED;
}
static rgb8_t glow_color(void) {
    if (ack_on)
        return color_of(ack_src, ack_player);
    if (look.kind != LOOK_RUN)
        return RED;
    if (look.source == SRC_WIFI && (look.act == ACT_NO_NET || look.act == ACT_STARTING))
        return src_color[SRC_WIFI];
    return color_of(look.source, look.player);
}
static uint8_t glow_level(uint32_t now) {
    if (ack_on) { /* two pulses, 450 ms each */
        uint32_t t = (now - ack_t0) % 450, x = (t < 225 ? t : 450 - t) * 256 / 225;
        return (uint8_t)(40 + 215 * ease(x) / 256);
    }
    switch (look.kind) {
    case LOOK_BOOT:
        return breathe(now, 2500, 25, 255);
    case LOOK_STANDBY:
        return 64;
    case LOOK_ERROR:
        return (now / 100) & 1 ? 0 : 255;
    default:
        break;
    }
    if (vol_on)
        return vol_limit ? ((now / 90) & 1 ? 40 : 255) : vol_level;
    switch (look.act) {
    case ACT_PLAYING:
        return 255;
    case ACT_READY:
        return breathe(now, 4000, 45, 210);
    case ACT_ASLEEP: /* dim, steady; at 36 a colour's weaker parts (amber's green) barely light: AUX asleep looks like
                      * standby's red [HW 2026-10-27] */
        return 90;
    case ACT_NO_NET:
        return breathe(now, 1600, 0, 200);
    case ACT_STARTING:
        return breathe(now, 2400, 20, 160);
    case ACT_CHOOSING:
        return breathe(now, 500, 110, 255);
    default:
        return 255;
    }
}
static uint16_t glow_scene(void) {
    return (uint16_t)(look.kind | look.act << 2 | look.source << 5 | look.player << 8 | vol_on << 11 | vol_limit << 12 |
                      ack_on << 13);
}
static void fade_start(uint32_t now, uint32_t ms) {
    for (int i = 0; i < 3; i++)
        from[i] = out[i];
    fade_t0 = now;
    fade_ms = ms;
}
static void glow_frame(uint32_t now) {
    uint16_t sc = glow_scene();
    if (sc != scene) {
        scene = sc;
        fade_start(now, look.kind == LOOK_ERROR ? 0 : ack_on ? 60 : vol_on ? 120 : FADE_MS);
    }
    rgb8_t c = glow_color();
    uint32_t lvl = glow_level(now);
    uint16_t tgt[3] = {(uint16_t)(c.r * lvl), (uint16_t)(c.g * lvl), (uint16_t)(c.b * lvl)}; /* perceptual << 8 */
    uint32_t e = now - fade_t0 >= fade_ms ? 256 : ease((now - fade_t0) * 256 / fade_ms);
    uint16_t v[3];
    for (int i = 0; i < 3; i++) {
        out[i] = (uint16_t)((int32_t)from[i] + ((int32_t)tgt[i] - (int32_t)from[i]) * (int32_t)e / 256);
        uint32_t p = out[i] >> 8;
        v[i] = (uint16_t)(p * p * PERIOD / 65025);
    }
    rgb(v);
}

/* ---- the API ---- */
void led_show(const led_look_t *l) { look = *l; }
void led_volume(uint8_t step, bool limit) {
    vol_on = true;
    vol_limit = limit;
    vol_until = tick_ms() + VOL_MS;
    vol_level = (uint8_t)(30 + 225u * step / VOL_MAX);
}
void led_ack(uint8_t source, uint8_t player) {
    ack_on = true;
    ack_src = source;
    ack_player = player;
    ack_t0 = tick_ms();
}
void led_flash(led_flash_t c, uint32_t ms) {
    overlay = true;
    overlay_c = c;
    overlay_until = tick_ms() + ms;
}
uint8_t led_code(void) { return (uint8_t)((look.source & 7) | (look.act & 7) << 3 | (look.kind & 3) << 6); }

void led_poll(uint32_t now) {
    if (overlay && tick_after(now, overlay_until)) {
        overlay = false;
        fade_start(now, FADE_MS);
    }
    if (vol_on && tick_after(now, vol_until))
        vol_on = false;
    if (ack_on && now - ack_t0 >= ACK_MS)
        ack_on = false;
    if (!tick_after(now, next_frame))
        return;
    next_frame = now + FRAME_MS;
    if (overlay) { /* 50 ms on, 50 ms off, like stock's flashes */
        rgb8_t c = flash_color[overlay_c];
        uint32_t on = (now / 50) & 1 ? 0 : 1;
        uint16_t v[3] = {(uint16_t)(c.r * c.r * PERIOD / 65025 * on), (uint16_t)(c.g * c.g * PERIOD / 65025 * on),
                         (uint16_t)(c.b * c.b * PERIOD / 65025 * on)};
        rgb(v);
        return;
    }
    glow_frame(now);
}
