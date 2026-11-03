#include "log.h"
#include "tick.h"
#include "inputs.h"
#include "settings.h"
#include "dsp.h"
#include "codec.h"
#include "amp.h"
#include "led.h"
#include "source.h"
#include "hostif.h"
#include "protect.h"
#include "usbaudio.h"
#include "udi_hid_cc.h"
#include "wifi.h"
#include "dm870.h"

/* a source silent this long: amp off until it has sound again (Prefs; 5 min unless set) */
static uint32_t source_amp_idle_ms(void) { return (uint32_t)g_settings.amp_idle_min * 60000u; }

#define AUX_SENSE_MS    350 /* stock FUN_80020400: S/PDIF lock poll in the AUX source */
#define RAMP_STEPS      5   /* stock FUN_800120ac: ~5 increments after unmute */
#define RAMP_STEP_MS    100
#define RAMP_FAST_MS    40   /* around a USB stream's (re)start */
#define CYCLE_COMMIT_MS 1000 /* stock timer 0x1a: switch 1 s after the last short ON/OFF press */
/* ours */
#define AMP_HOLD_MS   30000 /* standby: the supply stays on (muted) this long, so a quick wake doesn't cycle it */
#define USER_GUARD_MS 8000  /* after a source picked by hand, nothing switches by itself */
/* a USB stream takes the speaker (or wakes it) after sounding this long without a break, so that a system sound
 * doesn't (an alert lasts up to ~1.4 s [HW 2026-10-27]) */
#define USB_TAKE_MS    3000
#define SETTLE_AUTO_MS 5000 /* a source that took over by itself, silent this long: back to the computer playing */
#define LEVEL_MS       50   /* the DSP's input meter read this often (AUX) */
/* Sound on a metered input: 12 dB over that source's own noise floor, which is learnt: it drops to the lowest reading
 * at once and creeps up ~1 dB per 24 s while the level stays above it (music's quiet moments pull it back down); the
 * threshold kept within -80..-40 dBFS. Measured with a MacBook on AUX: floor -86 dBFS (a spike to -76), music at its
 * 15 % volume -66..-47 [HW 2026-10-27]; a hummy or hissy source sits higher, and gets a higher threshold. */
#define LEVEL_LOW    839   /* -80 dBFS (5.23): the threshold's floor */
#define LEVEL_HIGH   83886 /* -40 dBFS: its ceiling */
#define LEVEL_GAP_MS 500   /* sounding: over the threshold within this */
/* after a source or input change the meter only learns the floor this long (the
 * DSP and a module starting read as sound: the amp woke at boot) */
#define LEVEL_SETTLE_MS 1000
#define READY_LOOK_MS   30000 /* silent this long: the LED stops breathing and dims (the amp still sleeps at 5 min) */
/* the source silent this long: to where something plays or can be heard (longer
 * than a USB stream's 10 s end, so the computer's next play is new) */
#define SETTLE_MS        15000
#define SETTLE_PAUSED_MS 60000 /* ... with a receiver paused in front (a phone call) */
#define SETTLE_GUARD_MS  30000 /* ... not within this of a choice made by hand (a pick waiting for play) */

typedef enum { RUN_SOFTSTART, RUN_RAMP, RUN_PLAY, RUN_ASLEEP } run_phase_t;

static sys_state_t state = SYS_BOOT;
static source_t cur = SRC_NONE;
static run_phase_t phase;
static uint32_t t_phase, t_aux, ramp_ms;
static uint8_t ramp_i, err_code;
static unsigned applied = DSP_VOL_MUTE; /* the DSP volume we last set */
static bool amp_allowed;    /* the interlock: allowed from boot unless the EEPROM says locked (Amp deny with keep) */
static bool manual_standby; /* ON/OFF put us in standby: plug events still wake */
static bool host_mute;      /* USB feature-unit mute (only while USB is the source) */
static inputs_t last_in;
static source_t cycle_to = SRC_NONE; /* short ON/OFF presses: candidate shown on the LED, not yet switched to */
static uint8_t cycle_player;         /* ... on Wi-Fi: which receiver */
static uint32_t cycle_at;
static bool onoff_in_run;  /* this ON/OFF press began in RUN (a press that woke us doesn't cycle/standby) */
static uint32_t t_entered; /* the current source (or standby) since */
static bool auto_entered;  /* ... switched to by itself (a stream), not by hand */
static bool next_auto;     /* the next enter_run is source_auto_select's */
static bool quiet_wifi;    /* the next enter_run onto Wi-Fi doesn't resume the sender (a pick, not a play) */
static bool next_asleep;   /* the next enter_run leaves the amp asleep (the source is silent: sound wakes it) */
static uint32_t t_user;    /* the last source change by hand (keys, remote, plugs, the host link) */
static uint32_t t_active;  /* the current source last had sound */
static int32_t level = -1; /* the DSP's input meter, last sample (-1: not read) */
static uint32_t t_level, t_level_read; /* over the threshold last / read last */
static uint32_t floor_lv[SRC_N];       /* the learnt noise floor per metered source (5.23; 0: not learnt yet) */
static uint32_t t_settle;              /* the meter decides nothing before this */

static const char *src_name(source_t s) {
    static const char *n[] = {"none", "usb", "aux", "wifi", "ipod"};
    return s < SRC_N ? n[s] : "?";
}
/* iPod: never (no dock support) */
static bool present(source_t s) {
    const inputs_t *in = inputs();
    return (s == SRC_USB && in->usb_vbus) || (s == SRC_AUX && in->aux_inserted) ||
           (s == SRC_WIFI && dm870_usable()); /* the module powered, and not known to speak another protocol */
}
/* the DSP's meter measures the selected input: read it for the source nothing else can tell about (AUX) */
static bool metered(void) { return state == SYS_RUN && cur == SRC_AUX; }
static bool level_sounding(void) { return metered() && level >= 0 && tick_ms() - t_level < LEVEL_GAP_MS; }
/* Does the source have sound? USB: the host sends some. Wi-Fi: our OS's receiver in front plays (reg 0xC6). AUX: the
 * meter, only while it is the source. */
static bool sounding(source_t s) {
    switch (s) {
    case SRC_USB:
        return usbaudio_sounding();
    case SRC_WIFI:
        return wifi_play_state() == WIFI_PLAY_PLAYING;
    case SRC_AUX:
        return s == cur && level_sounding();
    default:
        return false;
    }
}
/* Is the source there, as a person sees it (playing or not)? A cable or a plug in; on the module a sender connected
 * (our OS lists them, reg 0xC6). */
static bool connected(source_t s) {
    switch (s) {
    case SRC_USB:
        return inputs()->usb_vbus;
    case SRC_AUX:
        return inputs()->aux_inserted;
    case SRC_WIFI:
        return dm870_native() && wifi_sessions() != 0;
    default:
        return false;
    }
}
/* Where to go when the source goes away: what plays (the computer, a receiver), the last source if it is still
 * there, then whatever is: AUX, a receiver with a sender (a phone paused is more recent use than a computer that
 * stays plugged in), the computer; NONE: nothing is. */
static source_t best_present(void) {
    source_t last = (source_t)g_settings.last_source;
    if (connected(SRC_USB) && usbaudio_sounding())
        return SRC_USB;
    if (wifi_play_state() == WIFI_PLAY_PLAYING)
        return SRC_WIFI;
    if (last > SRC_NONE && last < SRC_N && connected(last))
        return last;
    if (connected(SRC_AUX))
        return SRC_AUX;
    if (connected(SRC_WIFI))
        return SRC_WIFI;
    if (connected(SRC_USB))
        return SRC_USB;
    return SRC_NONE;
}
/* ... and when a person wakes it (a key, power-up): the same, else the last source if it can be selected (Wi-Fi:
 * the module powered and not known to speak another protocol), else Wi-Fi */
static source_t wake_target(void) {
    source_t s = best_present(), last = (source_t)g_settings.last_source;
    if (s != SRC_NONE)
        return s;
    if (last > SRC_NONE && last < SRC_N && present(last))
        return last;
    return present(SRC_WIFI) ? SRC_WIFI : SRC_NONE;
}
int32_t source_level(void) { return metered() ? level : 0; }
uint32_t source_level_floor(void) { return metered() ? floor_lv[cur] : 0; }
static uint32_t level_threshold(void) {
    uint32_t t = floor_lv[cur] * 4; /* +12 dB */
    return t < LEVEL_LOW ? LEVEL_LOW : t > LEVEL_HIGH ? LEVEL_HIGH : t;
}
static void level_sample(uint32_t now) {
    level = dsp_level();
    if (level < 0)
        return;
    uint32_t *f = &floor_lv[cur], v = (uint32_t)level;
    if (!*f || v < *f)
        *f = v < 16 ? 16 : v; /* down at once (16: ~-114 dBFS, so it can rise again) */
    else
        *f += *f >> 12; /* up ~0.002 dB a sample */
    if (tick_after(now, t_settle) && v >= level_threshold())
        t_level = now;
}
static unsigned target_index(void) { /* user step -> DSP index, under the heat cap */
    uint8_t v = g_settings.vol_step, cap = protect_heat_cap();
    return host_mute && cur == SRC_USB ? DSP_VOL_MUTE : DSP_VOL_MUTE - (v < cap ? v : cap);
}
static void set_vol(unsigned idx) {
    applied = idx;
    dsp_set_volume_index(idx);
}
/* May the amp be on? Every supply-on and unmute asks: allowed (the interlock: Amp deny), no protection tripped (ERROR
 * waits for ON/OFF), a source running and not asleep for silence (the idle rule) */
static bool amp_may_run(void) { return amp_allowed && state == SYS_RUN && phase != RUN_ASLEEP; }
/* the amp up from asleep (sound came, or a person asked): the normal sequence; t_active from now */
static void amp_wake(uint32_t now, const char *why) {
    if (state != SYS_RUN || phase != RUN_ASLEEP)
        return;
    log_puts("[src] ");
    log_puts(why);
    log_puts(": amp on\n");
    t_active = now;
    phase = RUN_SOFTSTART;
    t_phase = now;
    ramp_ms = RAMP_STEP_MS;
    if (amp_may_run())
        amp_supply_on();
}
/* The amp off until there is sound (hold_ms: the supply stays on that long, muted). The meter reads before the
 * volume stage [HW 2026-10-27: the same at steps 10 and 25], so muting the DSP doesn't blind it. */
static void amp_sleep(uint32_t hold_ms) {
    codec_dac_mute(true);
    set_vol(DSP_VOL_MUTE);
    amp_release(hold_ms);
    phase = RUN_ASLEEP;
}

static void route(source_t s) {
    switch (s) {
    case SRC_USB:
        dsp_set_input(DSP_IN_USB);
        break;
    case SRC_IPOD:
        dsp_set_input(DSP_IN_USB); /* no dock support: a dock's digital audio would arrive like USB's */
        break;
    case SRC_WIFI:
        dsp_set_input(DSP_IN_MODULE); /* the module's I2S (it drives the clocks) */
        break;
    case SRC_AUX:
        if (dsp_spdif_locked() == 1)
            dsp_set_input(DSP_IN_OPTICAL);
        else {
            dsp_set_input(DSP_IN_AUX);
            codec_aux_mode();
        }
        break;
    default:
        break;
    }
}

static void announce(void) {
    static const char *sn[] = {"boot", "standby", "run", "error"};
    log_puts("[src] ");
    log_puts(sn[state]);
    if (state == SYS_RUN) {
        log_putc(' ');
        log_puts(src_name(cur));
        if (auto_entered)
            log_puts(" (by itself)");
    }
    if (state == SYS_ERROR) {
        log_puts(" code ");
        log_dec(err_code);
    }
    log_putc('\n');
}
static void enter_standby(void) {
    if (cur == SRC_WIFI)
        wifi_leave();     /* stock: DE + E1 */
    codec_dac_mute(true); /* stock FUN_800138f0 */
    set_vol(DSP_VOL_MUTE);
    amp_release(AMP_HOLD_MS); /* stock: off at once; ours holds the supply a while */
    state = SYS_STANDBY;
    cur = SRC_NONE;
    auto_entered = next_auto = false;
    t_entered = tick_ms();
    announce();
}
static void enter_run(source_t s, uint32_t now) {
    if (state == SYS_ERROR)
        return; /* a tripped protection: only ON/OFF (or standby) leaves it */
    if (s == SRC_NONE) {
        enter_standby();
        return;
    }
    if (cur == SRC_WIFI && s != SRC_WIFI && state == SYS_RUN)
        wifi_leave(); /* stock: DE + E1 */
    bool to_wifi = s == SRC_WIFI && !(cur == SRC_WIFI && state == SYS_RUN);
    codec_dac_mute(true); /* stock FUN_800138f0 on every source switch */
    set_vol(DSP_VOL_MUTE);
    amp_mute();
    route(s);
    cur = s;
    state = SYS_RUN;
    manual_standby = false;
    cycle_to = SRC_NONE;
    auto_entered = next_auto;
    next_auto = false;
    phase = RUN_SOFTSTART;
    t_phase = now;
    t_aux = now + AUX_SENSE_MS;
    ramp_ms = RAMP_STEP_MS;
    t_entered = t_active = now;
    if (next_asleep)
        amp_sleep(AMP_HOLD_MS); /* silent: the amp waits for sound (a running supply held) */
    else if (amp_may_run())
        amp_supply_on();
    next_asleep = false;
    if (g_settings.last_source != s) {
        g_settings.last_source = s;
        settings_changed();
    }
    announce();
    if (to_wifi) {
        dm870_start();
        wifi_enter(!quiet_wifi);
    } /* stock: DF + E2 (module brought up if it is off) */
    quiet_wifi = false;
}
/* switched to by hand: nothing switches by itself for a while */
static void by_hand(void) { t_user = tick_ms(); }

/* Only a source that is there (USB and AUX unplugged: source_poll would switch away at once; the iPod never), and
 * not after a protection trip */
bool source_select(source_t s) {
    if (state == SYS_ERROR || !present(s))
        return false;
    by_hand();
    enter_run(s, tick_ms());
    return true;
}
void source_auto_select(source_t s) {
    if (state == SYS_ERROR || !present(s))
        return;
    next_auto = true;
    next_asleep = !sounding(s); /* (AUX's meter can't tell yet: asleep, its sound wakes it) */
    quiet_wifi = next_asleep;   /* onto a silent Wi-Fi by itself: no DF, a paused phone stays so */
    enter_run(s, tick_ms());
}
/* A person picked a source (plugged it in, chose the speaker on a phone or in an app): switch now, as a choice made
 * by hand, show it on the LED, and have the amp up (its soft start runs while the sender gets going), whether or not
 * anything plays yet. Picking the speaker doesn't make the sender play (Wi-Fi is entered without DF). */
void source_intent(source_t s, uint8_t player) {
    uint32_t now = tick_ms();
    if (state == SYS_ERROR || !present(s))
        return;
    log_puts("[src] picked: ");
    log_puts(s == SRC_WIFI ? "a receiver on the module" : src_name(s));
    log_putc('\n');
    by_hand();
    if (s != cur || state != SYS_RUN) {
        quiet_wifi = true;
        enter_run(s, now);
    } else {
        t_active = now; /* waiting for it to play: not idle */
        amp_wake(now, "picked");
    }
    led_ack(s, player);
}
bool source_amp_allowed(void) { return amp_allowed; }
void source_amp_allow(bool allow, bool keep) {
    if (keep) {
        uint8_t f = allow ? g_settings.flags & ~SETTINGS_AMP_LOCKED : g_settings.flags | SETTINGS_AMP_LOCKED;
        if (f != g_settings.flags) {
            g_settings.flags = f;
            settings_changed();
        }
    }
    if (allow) {
        amp_allowed = true;
        if (amp_may_run()) {
            codec_dac_mute(true);
            set_vol(DSP_VOL_MUTE);
            amp_supply_on();
            phase = RUN_SOFTSTART;
            t_phase = tick_ms();
            log_puts("amp ALLOWED: powering up, unmute after the soft start\n");
        } else
            log_puts("amp ALLOWED (powers up when a source has sound)\n");
    } else {
        amp_allowed = false;
        amp_off();
        codec_dac_mute(true);
        log_puts("amp locked off\n");
    }
    if (keep)
        log_puts(allow ? "amp: allowed from every boot (EEPROM)\n" : "amp: locked at every boot (EEPROM)\n");
}
bool source_amp_asleep(void) { return state == SYS_RUN && phase == RUN_ASLEEP; }
uint32_t source_silent_ms(uint32_t now) { return state == SYS_RUN && !sounding(cur) ? now - t_active : 0; }
void source_standby(void) {
    by_hand();
    manual_standby = true;
    enter_standby();
}
uint8_t source_error_code(void) { return err_code; }
sys_state_t source_state(void) { return state; }
source_t source_current(void) { return cur; }
void source_error(uint8_t code) {
    if (cur == SRC_WIFI && state == SYS_RUN)
        wifi_leave();
    err_code = code;
    codec_dac_mute(true);
    set_vol(DSP_VOL_MUTE);
    amp_off();
    state = SYS_ERROR;
    announce();
}

void source_init(void) {
    last_in = *inputs();
    amp_allowed = !(g_settings.flags & SETTINGS_AMP_LOCKED);
    if (!amp_allowed)
        log_puts("[src] amp locked from boot (EEPROM)\n");
    dsp_set_bass(g_settings.bass);
    source_t s = wake_target();
    if (s == SRC_NONE) {
        enter_standby();
        return;
    }
    next_asleep = true; /* a power-up: the amp waits for sound */
    enter_run(s, tick_ms());
}

/* Every volume change comes here: the keys, the remote, the host link, the USB host, the module. The volume cap
 * (Prefs, 40 unless set) is the one limit on all of them; the heat cap (protect.c) holds the volume lower while hot.
 * flash: LED feedback (keys, IR, the host link's Key; not the USB host or AirPlay). */
static void volume_step(int delta, bool flash) {
    int v = (int)g_settings.vol_step + delta, max = g_settings.vol_cap;
    bool limit = v < 0 || v > max;
    if (v < 0)
        v = 0;
    if (v > max)
        v = max;
    if (v != g_settings.vol_step) {
        g_settings.vol_step = (uint8_t)v;
        settings_changed();
    }
    usbaudio_local_volume((uint8_t)v); /* the host reads it back */
    wifi_local_volume();               /* stock: every local volume change goes to the module */
    log_puts("[src] vol ");
    log_dec(g_settings.vol_step);
    log_putc('\n');
    if (state == SYS_RUN && phase == RUN_PLAY)
        set_vol(target_index());
    if (flash)
        led_volume((uint8_t)v, limit || v == 0 || v == max);
}
/* The USB host's volume (its slider). A volume key steps here, and the host hears of it by USB audio's status
 * interrupt (usbaudio_local_volume): its slider follows. */
void source_host_volume(uint8_t step) {
    if (step != g_settings.vol_step)
        volume_step((int)step - (int)g_settings.vol_step, false);
}

void source_bass_set(uint8_t bass) {
    if (bass > BASS_MAX)
        bass = BASS_MAX;
    if (bass != g_settings.bass) {
        g_settings.bass = bass;
        settings_changed();
    }
    dsp_set_bass(bass);
}
/* a key from the host link, through the input queue like a real one; ON/OFF is a click (press + short release) or a
 * hold (press + hold) */
void source_key(uint8_t key) {
    if (key == KEY_ONOFF_SHORT || key == KEY_ONOFF_HOLD)
        inputs_inject_key(KEY_ONOFF);
    inputs_inject_key((key_event_t)key);
}
void source_volume_set(uint8_t step) {
    if (step != g_settings.vol_step)
        volume_step((int)step - (int)g_settings.vol_step, false);
}
void source_host_mute(bool m) {
    host_mute = m;
    if (state == SYS_RUN && cur == SRC_USB && phase == RUN_PLAY)
        set_vol(target_index());
}
/* USB's engine (re)starts (a stream opens, the rate changes): mute now, ramp back up quickly */
void source_ramp_in(void) {
    if (state != SYS_RUN || cur != SRC_USB || (phase != RUN_PLAY && phase != RUN_RAMP))
        return;
    set_vol(DSP_VOL_MUTE);
    phase = RUN_RAMP;
    ramp_i = 0;
    ramp_ms = RAMP_FAST_MS;
    t_phase = tick_ms();
}

/* media keys go to the USB host as HID consumer reports while USB is the source */
static void wake(void);
static void media_key(uint8_t bit, const char *name) {
    if (state == SYS_STANDBY && bit == HID_CC_PLAYPAUSE) { /* play in standby: wake and play (Wi-Fi: DF resumes) */
        log_puts("[src] play in standby: waking\n");
        wake();
        if (state == SYS_RUN && cur == SRC_USB)
            hid_cc_press(bit);
        return;
    }
    if (state == SYS_RUN && cur == SRC_WIFI) { /* to the module's receiver in front (0xD3) */
        wifi_key(bit == HID_CC_PLAYPAUSE ? ZD1_NATIVE_KEY_PLAY_PAUSE
                 : bit == HID_CC_NEXT    ? ZD1_NATIVE_KEY_NEXT
                                         : ZD1_NATIVE_KEY_PREV);
        log_puts("[src] key ");
        log_puts(name);
        log_puts(" -> the module's receiver in front\n");
        return;
    }
    bool sent = state == SYS_RUN && cur == SRC_USB && hid_cc_press(bit);
    log_puts("[src] key ");
    log_puts(name);
    log_puts(sent ? " -> USB host\n" : " (no USB host)\n");
}

/* What a person calls a source: the computer (USB), AUX, the dock, and each of the network module's receivers. One
 * counts while it is connected, playing or not (connected(): our OS says which receiver has a sender, reg 0xC6). In
 * stock's order (table 0x80011030: iPod, USB, Wi-Fi, AUX), Wi-Fi's receivers in the middle. */
typedef struct {
    source_t src;
    uint8_t player;
} choice_t;
static const choice_t order[] = {
    {SRC_IPOD, 0},
    {SRC_USB, 0},
    {SRC_WIFI, ZD1_PLAYER_AIRPLAY},
    {SRC_WIFI, ZD1_PLAYER_SPOTIFY},
    {SRC_WIFI, ZD1_PLAYER_SNAPCAST},
    {SRC_AUX, 0},
};
#define N_ORDER (sizeof order / sizeof order[0])
static bool available(choice_t c) {
    if (c.src != SRC_WIFI)
        return connected(c.src);
    return dm870_native() && (wifi_sessions() >> c.player) & 1;
}
static choice_t current(void) {
    choice_t c = {cur, 0};
    if (cur == SRC_WIFI)
        c.player = wifi_player(0);
    return c;
}
static const char *choice_name(choice_t c) {
    return c.src == SRC_WIFI && c.player != ZD1_PLAYER_NONE ? wifi_player_name(c.player) : src_name(c.src);
}
/* the next available one after `from` (from itself if there is none) */
static choice_t next_in_cycle(choice_t from) {
    unsigned i = 0;
    while (i < N_ORDER && !(order[i].src == from.src && (from.src != SRC_WIFI || order[i].player == from.player)))
        i++;
    if (i == N_ORDER)
        i = from.src == SRC_WIFI ? 1 : 0; /* Wi-Fi with no receiver in front: from USB's place */
    for (unsigned n = 1; n <= N_ORDER; n++) {
        choice_t c = order[(i + n) % N_ORDER];
        if (available(c))
            return c;
    }
    return from;
}
/* stock FUN_800237c0 / FUN_800247a8: a short press steps to the next available source and shows it on the LED; the
 * switch happens 1 s after the last press. Nothing else available: nothing happens. */
static void cycle_step(uint32_t now) {
    choice_t from = cycle_to != SRC_NONE ? (choice_t){cycle_to, cycle_player} : current();
    choice_t c = next_in_cycle(from);
    if (c.src == from.src && c.player == from.player) {
        log_puts("[src] ON/OFF: nothing else to choose\n");
        return;
    }
    cycle_to = c.src;
    cycle_player = c.player;
    cycle_at = now + CYCLE_COMMIT_MS;
    log_puts("[src] next source: ");
    log_puts(choice_name(c));
    log_putc('\n');
}
/* a choice made by hand: the source, and on Wi-Fi the receiver (it resumes its sender, the others pause) */
static void choose(choice_t c, uint32_t now) {
    choice_t was = current();
    by_hand();
    if (c.src != cur || state != SYS_RUN)
        enter_run(c.src, now);
    if (c.src == SRC_WIFI && c.player && !(was.src == SRC_WIFI && was.player == c.player))
        wifi_select(c.player);
}
static void cycle_poll(uint32_t now) {
    if (cycle_to == SRC_NONE)
        return;
    if (state != SYS_RUN) {
        cycle_to = SRC_NONE;
        return;
    }
    if (!tick_after(now, cycle_at))
        return;
    choice_t c = {cycle_to, cycle_player};
    cycle_to = SRC_NONE;
    if (available(c))
        choose(c, now);
}
bool source_select_player(uint8_t player) {
    choice_t c = {SRC_WIFI, player};
    if (state == SYS_ERROR || !available(c))
        return false;
    choose(c, tick_ms());
    return true;
}

static void wake(void) {
    source_t s = wake_target();
    if (s != SRC_NONE) {
        by_hand();
        enter_run(s, tick_ms());
    }
}

static void handle_key(key_event_t k) {
    switch (k) {
    case KEY_ONOFF: /* press: wakes / acknowledges; in RUN wait for the release */
        onoff_in_run = state == SYS_RUN;
        if (state == SYS_ERROR)
            enter_standby(); /* acknowledge a protection trip */
        else if (state == SYS_STANDBY)
            wake();
        break;
    case KEY_ONOFF_SHORT:
        if (onoff_in_run && state == SYS_RUN)
            cycle_step(tick_ms());
        onoff_in_run = false;
        break;
    case KEY_ONOFF_HOLD:
        if (onoff_in_run && state == SYS_RUN) {
            cycle_to = SRC_NONE;
            source_standby();
        }
        onoff_in_run = false;
        break;
    case KEY_STANDBY: /* IR standby: toggle */
        if (state == SYS_ERROR)
            enter_standby();
        else if (state == SYS_RUN)
            source_standby();
        else if (state == SYS_STANDBY)
            wake();
        break;
    /* a person at the speaker wants sound: these wake a sleeping amp too (a source too quiet for the meter) */
    case KEY_VOLUP:
        if (state == SYS_RUN) {
            amp_wake(tick_ms(), "volume key");
            volume_step(+1, true);
        }
        break;
    case KEY_VOLDN:
        if (state == SYS_RUN) {
            amp_wake(tick_ms(), "volume key");
            volume_step(-1, true);
        }
        break;
    case KEY_PLAYPAUSE:
        amp_wake(tick_ms(), "play key");
        media_key(HID_CC_PLAYPAUSE, "play/pause");
        break;
    case KEY_NEXT:
        amp_wake(tick_ms(), "next key");
        media_key(HID_CC_NEXT, "next");
        break;
    case KEY_PREV:
        amp_wake(tick_ms(), "prev key");
        media_key(HID_CC_PREV, "prev");
        break;
    default:
        break; /* rear key: setup (unused) */
    }
}

/* Is anything but the computer connected (that a beep from it could take the speaker from)? */
static bool others_connected(void) { return connected(SRC_AUX) || connected(SRC_WIFI); }

/* Newest wins: a USB stream that starts takes the speaker or wakes it (once it has sounded USB_TAKE_MS without a
 * break; at once while the speaker runs and nothing else is connected). The receivers take it through the play state
 * (wifi.c), AUX by being plugged in. Never within USER_GUARD_MS of a choice made by hand; a stream that was already
 * playing when the current source began doesn't count as new (settle() goes back to it once the source is silent). */
static void auto_switch(uint32_t now) {
    uint32_t since;
    if (g_settings.flags & SETTINGS_NO_AUTO_SWITCH)
        return;
    if (state == SYS_RUN ? (cur == SRC_USB || cycle_to != SRC_NONE) : state != SYS_STANDBY)
        return;
    if (!tick_after(now, t_user + USER_GUARD_MS) || !present(SRC_USB))
        return;
    if (!usbaudio_session(&since) || !usbaudio_sounding())
        return;
    if (usbaudio_sounding_ms(now) < (state == SYS_RUN && !others_connected() ? 1 : USB_TAKE_MS))
        return;
    if (tick_after(since, t_entered)) {
        log_puts(state == SYS_STANDBY ? "[src] a USB stream: waking\n"
                                      : "[src] a USB stream started: taking the speaker\n");
        source_auto_select(SRC_USB);
    }
}

/* The source silent long enough: go where something plays or can be heard. The computer playing first (a source that
 * took over by itself gives it back after SETTLE_AUTO_MS: AirPlay interrupting the computer); else AUX if plugged in
 * (the meter hears it only while it is the source); else, if the source isn't there any more (its sender left), the
 * computer if connected. Never within SETTLE_GUARD_MS of a choice made by hand; entered with the amp asleep unless
 * the new source plays. */
static void settle(uint32_t now) {
    uint8_t st = 0;
    if (state != SYS_RUN || cycle_to != SRC_NONE || sounding(cur) || !tick_after(now, t_user + SETTLE_GUARD_MS))
        return;
    bool paused = cur == SRC_WIFI && wifi_player(&st) && st == ZD1_PLAYER_PAUSED;
    uint32_t wait = paused ? SETTLE_PAUSED_MS : SETTLE_MS;
    source_t to = SRC_NONE;
    if (cur != SRC_USB && connected(SRC_USB) && usbaudio_sounding()) {
        to = SRC_USB;
        if (auto_entered)
            wait = SETTLE_AUTO_MS;
    } else if (cur != SRC_AUX && connected(SRC_AUX))
        to = SRC_AUX;
    else if (cur != SRC_USB && !connected(cur) && connected(SRC_USB))
        to = SRC_USB;
    if (to == SRC_NONE || now - t_active < wait)
        return;
    log_puts(to == SRC_AUX         ? "[src] nothing else plays: back to AUX\n"
             : usbaudio_sounding() ? "[src] silent: back to the computer, which plays\n"
                                   : "[src] the source went away: to the computer\n");
    source_auto_select(to);
}

/* the LED: what the speaker does (led.c draws it); our OS names its receiver */
static void show(void) {
    led_look_t l = {LOOK_RUN, cur, ZD1_PLAYER_NONE, ACT_PLAYING};
    if (state == SYS_BOOT)
        l.kind = LOOK_BOOT;
    else if (state == SYS_STANDBY)
        l.kind = LOOK_STANDBY;
    else if (state == SYS_ERROR)
        l.kind = LOOK_ERROR;
    else {
        source_t s = cycle_to != SRC_NONE ? cycle_to : cur;
        l.source = s;
        if (cycle_to != SRC_NONE)
            l.act = ACT_CHOOSING;
        else if (s == SRC_WIFI && !dm870_ready())
            l.act = ACT_STARTING;
        else if (s == SRC_WIFI && !wifi_net_connected())
            l.act = ACT_NO_NET;
        else if (phase == RUN_ASLEEP)
            l.act = ACT_ASLEEP;
        else
            l.act = sounding(s) ? ACT_PLAYING : tick_ms() - t_active < READY_LOOK_MS ? ACT_READY : ACT_ASLEEP;
        if (cycle_to == SRC_WIFI)
            l.player = cycle_player;
        else if (s == SRC_WIFI)
            l.player = wifi_player(0);
    }
    led_show(&l);
}

void source_poll(uint32_t now) {
    const inputs_t *in = inputs();
    key_event_t k;
    while ((k = inputs_key()) != KEY_NONE) {
        if (!hostif_key(k))
            handle_key(k); /* the gate first */
    }
    cycle_poll(now);

    /* plug / unplug events (from any state but ERROR) */
    if (state != SYS_ERROR && state != SYS_BOOT) {
        if (in->usb_vbus && !last_in.usb_vbus)
            source_intent(SRC_USB, 0);
        else if (in->aux_inserted && !last_in.aux_inserted) {
            floor_lv[SRC_AUX] = 0;
            source_intent(SRC_AUX, 0);
        } /* a new source: learn its floor */
        else if (state == SYS_RUN && !present(cur) && (cur == SRC_USB || cur == SRC_AUX)) { /* its plug came out */
            source_t to = best_present();
            if (to == SRC_NONE)
                enter_standby();
            else
                source_auto_select(to);
        }
    }
    last_in = *in;
    if (usbaudio_take_pick() && present(SRC_USB))
        source_intent(SRC_USB, 0);
    if (usbaudio_take_open() && state == SYS_RUN) { /* the computer opened its stream: sound is likely coming */
        if (cur == SRC_USB && phase == RUN_ASLEEP)
            amp_wake(now, "the computer opened the stream");
        else if (cur != SRC_USB && cycle_to == SRC_NONE && present(SRC_USB) && !others_connected() &&
                 !(g_settings.flags & SETTINGS_NO_AUTO_SWITCH) && tick_after(now, t_user + USER_GUARD_MS)) {
            log_puts("[src] the computer opened the stream and nothing else is connected: to USB\n");
            source_auto_select(SRC_USB); /* its amp starts now */
        }
    }
    { /* a new metered input: let it settle first */
        static bool was_metered;
        static source_t was_cur;
        if (metered() != was_metered || cur != was_cur) {
            was_metered = metered();
            was_cur = cur;
            t_settle = now + LEVEL_SETTLE_MS;
            t_level = now - LEVEL_GAP_MS;
        }
    }
    if (metered() && tick_after(now, t_level_read + LEVEL_MS)) {
        t_level_read = now;
        level_sample(now);
    }
    auto_switch(now);
    settle(now);

    if (state == SYS_RUN) {
        if (sounding(cur))
            t_active = now;
        switch (phase) {
        case RUN_SOFTSTART: { /* unmute once amp.c's soft start allows it (asked every pass until it does) */
            bool may = amp_may_run();
            if (may && !amp_unmute())
                break;
            if (may)
                codec_dac_mute(false); /* stock FUN_80013a68: amp unmute, then codec DAC unmute */
            phase = RUN_RAMP;
            ramp_i = 0;
            t_phase = now;
            break;
        }
        case RUN_RAMP:
            if (tick_after(now, t_phase + ramp_ms)) {
                ramp_i++;
                t_phase = now;
                unsigned tgt = target_index();
                set_vol(DSP_VOL_MUTE - (DSP_VOL_MUTE - tgt) * ramp_i / RAMP_STEPS);
                if (ramp_i >= RAMP_STEPS) {
                    phase = RUN_PLAY;
                    ramp_ms = RAMP_STEP_MS;
                }
            }
            break;
        case RUN_PLAY:
            if (target_index() != applied)
                set_vol(target_index()); /* the heat cap moved */
            if (now - t_active >= source_amp_idle_ms()) {
                amp_sleep(0);
                log_puts("[src] silent for ");
                log_dec(g_settings.amp_idle_min);
                log_puts(" min: amp off until there is sound\n");
            }
            break;
        case RUN_ASLEEP:
            if (sounding(cur))
                amp_wake(now, "sound");
            break;
        }
        if (cur == SRC_AUX && tick_after(now, t_aux)) { /* stock AUX auto-sense: optical <-> analog */
            t_aux = now + AUX_SENSE_MS;
            dsp_input_t want = dsp_spdif_locked() == 1 ? DSP_IN_OPTICAL : DSP_IN_AUX;
            if (want != dsp_input()) {
                dsp_set_input(want);
                if (want == DSP_IN_AUX)
                    codec_aux_mode();
                log_puts(want == DSP_IN_OPTICAL ? "[src] aux: optical (S/PDIF lock)\n" : "[src] aux: analog\n");
            }
        }
    }
    show();
}
