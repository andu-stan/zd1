/* The module as the speaker's Wi-Fi source: one picture of it (network, play state, our OS's receivers, the
 * volume it asked for), filled by our OS over the native registers (reg 0xD5 = this firmware's protocol,
 * dm870_native(); docs/protocol/sequences.md, Our OS): the speaker as one snapshot (0xD2: the volume applied, the
 * cap, whether the module is the input) written at every hello and change, key presses (0xD3), its volume requests
 * (0xD4), its play state from its receivers (0xC6). Fed from dm870.c's notification drain; driven from the main
 * loop. A module that speaks anything else (the stock app, another protocol) is no source: dm870.c reports it. */
#include <string.h>
#include "log.h"
#include "tick.h"
#include "dm870.h"
#include "source.h"
#include "usbaudio.h"
#include "wifi.h"
#include "settings.h"

static bool net_up;
static wifi_play_t play;
static int16_t mod_step = -1;        /* a volume the module asked for, for when it is the input (-1 = none) */
static bool from_module;             /* a volume change the module asked for is being applied */
static bool input;                   /* the module is the speaker's input: between wifi_enter and wifi_leave */
static uint8_t player, player_state; /* our OS's receiver in front (0xC6) */
static uint8_t sessions;             /* ... the receivers with a sender connected: bit (ZD1_PLAYER_*) */
static uint8_t keys;                 /* counts the key presses (0xD3 b1) */
static uint8_t shown[4];             /* the last snapshot written (0xD2) */
static uint8_t chosen;               /* the receiver chosen since the module became the input (0 = none) */

bool wifi_net_connected(void) { return net_up; }
wifi_play_t wifi_play_state(void) { return play; }
uint8_t wifi_player(uint8_t *state) {
    if (state)
        *state = player_state;
    return player;
}
uint8_t wifi_sessions(void) { return sessions; }
const char *wifi_player_name(uint8_t p) {
    static const char *nm[] = {"none", "AirPlay", "Spotify", "Snapcast", "an app"};
    return nm[p > ZD1_PLAYER_OTHER ? ZD1_PLAYER_OTHER : p];
}
int wifi_pending_volume(void) { return mod_step; }
void wifi_select(uint8_t p) {
    if (!dm870_native() || p == chosen)
        return; /* once per entry (wifi_enter's, then choose's) */
    chosen = p;
    dm870_reg_write(ZD1_REG_SELECT, p, 0x00);
    log_puts("[wifi] chose receiver ");
    log_dec(p);
    log_putc('\n');
}

/* the speaker as our OS should see it; `always`: even if it didn't change (a hello, an answer to 0xD4) */
static void snapshot(bool always) {
    if (!dm870_native())
        return;
    uint8_t s[4] = {ZD1_NATIVE_PROTO, (uint8_t)(mod_step >= 0 && !input ? mod_step : g_settings.vol_step),
                    usbaudio_volmax(), input ? ZD1_NATIVE_INPUT : 0};
    if (!always && !memcmp(s, shown, sizeof s))
        return;
    memcpy(shown, s, sizeof s);
    dm870_reg_write_v(ZD1_REG_SPEAKER, s, sizeof s);
}

static void apply_module_volume(void) {
    if (mod_step < 0 || !input)
        return;
    uint8_t s = (uint8_t)mod_step;
    mod_step = -1;
    from_module = true;
    source_volume_set(s);
    from_module = false;
}

/* the module asks for a volume (0xD4): applied while it is the input, else kept for when it is; capped like the USB
 * host's volume */
static void module_volume(uint8_t step) {
    if (step > usbaudio_volmax())
        step = usbaudio_volmax(); /* the volume cap, as everywhere */
    mod_step = step;
    apply_module_volume();
    snapshot(true); /* what was applied, even if nothing changed */
}

static void auto_switch(void) { /* a receiver plays -> Wi-Fi (wakes from sleep) */
    sys_state_t st = source_state();
    if ((st == SYS_RUN && source_current() != SRC_WIFI) || st == SYS_STANDBY) {
        log_puts("[wifi] a receiver started playing -> switching to Wi-Fi\n");
        source_auto_select(SRC_WIFI);
    }
}

static void set_play(wifi_play_t p) {
    if (p == play)
        return;
    static const char *nm[] = {"unknown", "empty", "playing", "paused"};
    play = p;
    log_puts("[wifi] play state ");
    log_puts(nm[p]);
    log_putc('\n');
    if (p == WIFI_PLAY_PLAYING)
        auto_switch();
}

void wifi_notify(uint8_t reg, const uint8_t *b, uint16_t n) {
    uint8_t b0 = n > 0 ? b[0] : 0, b1 = n > 1 ? b[1] : 0;
    switch (reg) {
    case ZD1_REG_NET_STATUS:
        net_up = zd1_net_connected(b0, b1);
        break;
    case ZD1_REG_INTENT: /* a person picked one of our receivers */
        if (n >= 2 && dm870_native())
            source_intent(SRC_WIFI, b0 > ZD1_PLAYER_OTHER ? ZD1_PLAYER_OTHER : b0);
        break;
    case ZD1_REG_PLAYER: { /* [front, its state, the receivers with a sender] */
        uint8_t s = n > 2 ? b[2] : 0;
        if (n < 2 || !dm870_native())
            break;
        set_play(b1 == ZD1_PLAYER_PLAYING  ? WIFI_PLAY_PLAYING
                 : b1 == ZD1_PLAYER_PAUSED ? WIFI_PLAY_PAUSED
                                           : WIFI_PLAY_EMPTY);
        if (b0 == player && b1 == player_state && s == sessions)
            break;
        player = b0 > ZD1_PLAYER_OTHER ? ZD1_PLAYER_OTHER : b0;
        player_state = b1;
        sessions = s;
        log_puts("[wifi] receiver ");
        log_puts(wifi_player_name(player));
        log_puts(b1 == ZD1_PLAYER_PLAYING ? " playing" : b1 == ZD1_PLAYER_PAUSED ? " paused" : " stopped");
        log_puts(", with a sender:");
        for (uint8_t p = ZD1_PLAYER_AIRPLAY; p <= ZD1_PLAYER_OTHER; p++)
            if (s & 1 << p) {
                log_putc(' ');
                log_puts(wifi_player_name(p));
            }
        log_putc('\n');
        break;
    }
    case ZD1_REG_VOLUME_SET: /* our OS asks for a volume */
        if (dm870_native() && n >= 1)
            module_volume(b0);
        break;
    default:
        break;
    }
}

/* the module (re)greeted (dm870.c): our OS gets the snapshot. The play state from before is forgotten: a receiver
 * that plays again after a restart (a Snapcast client reconnects by itself) reports "playing" first, and that must
 * count as a stream starting, not as no change [HW 2026-10-29: after `zd1 update` restarted the module, Snapcast
 * played and the speaker stayed on USB] */
void wifi_module_ready(void) {
    play = WIFI_PLAY_UNKNOWN;
    player = player_state = sessions = 0;
    input = source_state() == SYS_RUN && source_current() == SRC_WIFI;
    snapshot(true);
}
/* resume: a person chose Wi-Fi (ON/OFF, the play key in standby): the receiver in front is chosen (0xC2: it resumes
 * its sender) */
void wifi_enter(bool resume) {
    input = true;
    chosen = ZD1_PLAYER_NONE;
    apply_module_volume();
    snapshot(false);
    if (resume && player != ZD1_PLAYER_NONE)
        wifi_select(player);
}
void wifi_leave(void) {
    input = false;
    chosen = ZD1_PLAYER_NONE;
    snapshot(false);
}
void wifi_key(uint8_t key) { /* a transport key while on Wi-Fi */
    if (!dm870_native())
        return;
    dm870_reg_write(ZD1_REG_KEY, key, ++keys);
}

void wifi_local_volume(void) { /* every volume change on the speaker (and the ones the module asked for) */
    if (!from_module)
        mod_step = -1; /* the knob (or the host) after the module's request: the knob wins */
    snapshot(false);
}
