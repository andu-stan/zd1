/* The host link (hostif.h; docs/protocol/host.md). The USB interrupt takes a request (CALL), hands out the log and the
 * reply (POLL) and answers HELLO; the main loop runs the request (hostif_poll), between its other work. Every request
 * gets its reply or Error, after the text it printed. One that waits for something (Unlock's press, Module POWER /
 * BL2 / HOLD, the module OS's answer to a setting or a record) is answered from hostif_poll when it ends, the main
 * loop going on meanwhile; the next request waits for it.
 * The gate: a gated request (zd1h_gated(): changes the speaker or reads its RAM) runs only after a press of ON/OFF on
 * the speaker, asked for by `Unlock`, and then for 10 minutes after the last gated one; or always, if the user opened
 * it for good (`Lock`, kept in the EEPROM). Whatever runs on the host can reach this port, a web page included. */
#include "conf_usb.h" /* first: the USB headers depend on it */
#include <string.h>
#include <sysclk.h>
#include <udd.h>
#include <udc.h>
#include <delay.h>
#include "hostif.h"
#include "udi_hostif.h"
#include "log.h"
#include "source.h"
#include "settings.h"
#include "amp.h"
#include "dm870.h"
#include "modboot.h"
#include "update.h"
#include "selfdfu.h"
#include "zd1_board.h"
#include "inputs.h"
#include "led.h"
#include "tick.h"
#include "modcon.h"
#include "protect.h"
#include "ir.h"
#include "dsp.h"
#include "usbaudio.h"
#include "wifi.h"
#include "udi_audio.h"
#include "udi_hid_cc.h"
#include "util.h"
#include "pipe.h"

static volatile bool polled;       /* a POLL since the main loop last looked */
static volatile bool hello_closed; /* a HELLO closed the module's console (for the log) */
static volatile uint32_t uptime;   /* the main loop's clock, for HELLO in the interrupt */
static uint32_t last_poll;

/* ---- requests and replies ---- */
#define DATA_MAX ZD1H_DATA_DATA_MAX /* a Data reply's bytes (Peek, ModRead) */
static uint8_t req[ZD1H_MSG_MAX];
static volatile uint16_t req_len, req_seq, cur_seq;
static volatile bool req_full; /* a request is in: CALL stalls until its reply is queued */
static uint8_t rep[ZD1H_MSG_MAX];
static volatile uint16_t rep_len, rep_seq;
static volatile uint32_t rep_after; /* the log position the reply goes out after */
static volatile bool rep_ready, rep_taken;
static uint8_t poll_buf[ZD1H_POLL_MAX];
static uint8_t hello_buf[ZD1H_MSG_MAX];

static void hello_fill(zd1h_hello_reply_t *h) {
    h->proto = ZD1H_VERSION;
    copy_str(h->fw, sizeof h->fw, g_fw_version, STR_ALL);
    copy_str(h->built, sizeof h->built, g_fw_built, STR_ALL);
    copy_str(h->board, sizeof h->board, g_fw_board, STR_ALL);
    h->uptime_ms = uptime;
    memcpy(h->requests, zd1h_requests, sizeof zd1h_requests);
    h->requests_len = sizeof zd1h_requests;
}

/* ---- the gate ---- */
#define GATE_MS    (10u * 60 * 1000) /* unlocked: until this long after the last gated request */
#define CONFIRM_MS 30000u            /* Unlock: how long it waits for the press */
static bool unlocked;                /* by a press, until unlock_until */
static uint32_t unlock_until;
static volatile bool confirm_wait, confirmed; /* an Unlock waits for the press (the main loop: inputs -> hostif_key) */
static bool module_wait;                      /* a Module request waits for its op (dm870_op_result) */
static bool query_wait;      /* a GetSetting / SetSetting / Record waits for the module's answer (dm870_query_result) */
static bool swallow_release; /* the press was ours: its release (short / hold) is too */
static uint32_t confirm_until;

uint8_t hostif_gate(uint16_t *secs_left) {
    *secs_left = 0;
    if (g_settings.flags & SETTINGS_GATE_OPEN)
        return ZD1H_GATE_OPEN;
    if (unlocked && tick_after(uptime, unlock_until))
        unlocked = false;
    if (!unlocked)
        return ZD1H_GATE_LOCKED;
    *secs_left = (uint16_t)((unlock_until - uptime) / 1000);
    return ZD1H_GATE_UNLOCKED;
}

void hostif_first_start(void) {
    uint32_t c = ~crc32_add(0xFFFFFFFFu, (const uint8_t *)g_fw_version, strlen(g_fw_version));
    uint16_t h = (uint16_t)(c ^ c >> 16);
    if (h == 0)
        h = 1; /* 0: no firmware recorded (a blank EEPROM, or one written before this field) */
    if (g_settings.fw_seen == h)
        return;
    g_settings.fw_seen = h;
    settings_changed();
    if (g_settings.flags & SETTINGS_GATE_OPEN)
        return;
    unlocked = true;
    unlock_until = tick_ms() + GATE_MS;
    log_puts("[gate] this firmware's first start: unlocked for 10 minutes (it was just flashed)\n");
}

bool hostif_key(uint8_t key) {
    if (confirm_wait && key == KEY_ONOFF) {
        confirmed = true;
        swallow_release = true;
        return true;
    }
    if (swallow_release && (key == KEY_ONOFF_SHORT || key == KEY_ONOFF_HOLD)) {
        swallow_release = false;
        return true;
    }
    return false;
}

/* POLL: the log first, then the reply once the text printed before it is out, then the module's console (while the
 * host has it open: it gets up to half when the log waits too). Never empty (an empty text record): ASF's control IN
 * path doesn't handle a zero-length data stage reliably. */
static uint16_t build_poll(uint16_t cap) {
    unsigned con = modcon_host_avail(), keep = con ? 3 + (con < cap / 2u ? con : cap / 2u) : 0;
    unsigned k = log_take(poll_buf + 3, cap - 3u - keep);
    poll_buf[0] = ZD1H_REC_TEXT;
    poll_buf[1] = (uint8_t)k;
    poll_buf[2] = (uint8_t)(k >> 8);
    uint16_t n = (uint16_t)(3 + k);
    if (rep_ready && (int32_t)(log_taken() - rep_after) >= 0 && n + 5u + rep_len <= cap) {
        uint16_t l = rep_len + 2;
        poll_buf[n++] = ZD1H_REC_REPLY;
        poll_buf[n++] = (uint8_t)l;
        poll_buf[n++] = (uint8_t)(l >> 8);
        poll_buf[n++] = (uint8_t)rep_seq;
        poll_buf[n++] = (uint8_t)(rep_seq >> 8);
        memcpy(poll_buf + n, rep, rep_len);
        n += rep_len;
        rep_ready = false;
        rep_taken = true;
    }
    if (con && n + 4u <= cap) {
        unsigned c = modcon_host_take(poll_buf + n + 3, cap - n - 3u);
        poll_buf[n] = ZD1H_REC_CONSOLE;
        poll_buf[n + 1] = (uint8_t)c;
        poll_buf[n + 2] = (uint8_t)(c >> 8);
        n += 3 + c;
    }
    return n;
}

static void call_received(void) {
    req_len = udd_g_ctrlreq.req.wLength;
    req_seq = udd_g_ctrlreq.req.wValue;
    req_full = true;
}

static bool hostif_setup(void) {
    if (Udd_setup_type() != USB_REQ_TYPE_VENDOR)
        return false;
    uint16_t len = udd_g_ctrlreq.req.wLength;
    switch (udd_g_ctrlreq.req.bRequest) {
    case ZD1H_REQ_CALL:
        if (!Udd_setup_is_out() || len == 0 || len > ZD1H_MSG_MAX || req_full)
            return false; /* a stall: busy */
        udd_set_setup_payload(req, len);
        udd_g_ctrlreq.callback = call_received;
        return true;
    case ZD1H_REQ_POLL:
        if (!Udd_setup_is_in() || len < 3)
            return false;
        polled = true;
        udd_set_setup_payload(poll_buf, build_poll(len < ZD1H_POLL_MAX ? len : ZD1H_POLL_MAX));
        return true;
    case ZD1H_REQ_HELLO: {
        if (!Udd_setup_is_in())
            return false;
        if (modcon_host()) { /* a new host: the last one's console isn't its */
            modcon_host_open(false);
            hello_closed = true;
        }
        zd1h_hello_reply_t h;
        hello_fill(&h);
        int n = zd1h_encode(&ZD1H_HELLO_REPLY, &h, hello_buf, sizeof hello_buf);
        if (n < 0)
            return false;
        udd_set_setup_payload(hello_buf, n < len ? (uint16_t)n : len);
        return true;
    }
    default:
        return false;
    }
}

static bool hostif_enable(void) {
    pipe_enable();
    return true;
}
static uint8_t hostif_getsetting(void) { return 0; }
UDC_DESC_STORAGE udi_api_t udi_api_hostif = {hostif_enable, pipe_disable, hostif_setup, hostif_getsetting, NULL};

/* ---- the main loop's side ---- */
static void reply(const zd1h_msg_t *m, const void *s) {
    int n = zd1h_encode(m, s, rep, sizeof rep);
    if (n < 0) {
        zd1h_error_t e = {ZD1H_ERR_ARG, "reply too long"};
        n = zd1h_encode(&ZD1H_ERROR, &e, rep, sizeof rep);
    }
    irqflags_t f = cpu_irq_save();
    rep_len = (uint16_t)n;
    rep_seq = cur_seq;
    rep_after = log_written();
    rep_taken = false;
    rep_ready = true;
    cpu_irq_restore(f);
}
static void error(uint8_t code, const char *text) {
    zd1h_error_t e;
    e.code = code;
    copy_str(e.text, sizeof e.text, text, STR_ALL);
    reply(&ZD1H_ERROR, &e);
}
/* for requests that end in a reset: the host has its reply before the device goes (the watchdog fed once more, so
 * that it doesn't come first) */
static void wait_taken(void) {
    selfdfu_watchdog_feed();
    uint32_t end = tick_ms() + 1000;
    while (!rep_taken && !tick_after(tick_ms(), end)) {
    }
    delay_ms(50); /* the status stage */
}

static void info_fill(zd1h_info_reply_t *r) {
    copy_str(r->fw, sizeof r->fw, g_fw_version, STR_ALL);
    copy_str(r->built, sizeof r->built, g_fw_built, STR_ALL);
    r->uptime_ms = uptime;
    r->rcause = (uint8_t)g_rcause;
    r->state = (uint8_t)source_state();
    r->source = (uint8_t)source_current();
    r->volume = g_settings.vol_step;
    r->bass = g_settings.bass;
    r->amp_allowed = source_amp_allowed();
    r->amp_on = amp_is_on();
    r->debug = dm870_debug();
    r->mod_state = dm870_state();
    r->mod_status = dm870_last_status();
    copy_str(r->mod_os, sizeof r->mod_os, dm870_os_version(), STR_ALL);
    copy_str(r->mod_ip, sizeof r->mod_ip, dm870_ip(), STR_ALL);
    copy_str(r->mod_name, sizeof r->mod_name, dm870_name(), STR_ALL);
    r->modcon_open = false; /* the CDC port: gone */
    r->gate = hostif_gate(&r->gate_s);
    r->amp_auto = !(g_settings.flags & SETTINGS_AMP_LOCKED);
    r->amp_asleep = source_amp_asleep();
    r->player = dm870_native() ? wifi_player(&r->player_state) : ZD1H_PLAYER_NONE;
    r->auto_switch = !(g_settings.flags & SETTINGS_NO_AUTO_SWITCH);
    r->players_on = dm870_native() ? wifi_sessions() : 0;
    r->mod_proto = dm870_proto();
    r->link_proto = ZD1_NATIVE_PROTO;
    r->amp_idle_s = (uint16_t)(g_settings.amp_idle_min * 60u);
    r->volume_cap = g_settings.vol_cap;
    r->console_open = modcon_host();
}

static void diag_fill(zd1h_diag_reply_t *r) {
    int adc[3];
    protect_state(adc, &r->n_temp, &r->n_adc4, &r->n_fault);
    r->adc4 = adc[0] < 0 ? 0 : (uint16_t)adc[0];
    r->adc5 = adc[1] < 0 ? 0 : (uint16_t)adc[1];
    r->adc6 = adc[2] < 0 ? 0 : (uint16_t)adc[2];
    r->fault = !gpio_get_pin_value(PIN_FAULT);
    r->amp_fault = !gpio_get_pin_value(PIN_AMP_FAULT);
    r->error = source_state() == SYS_ERROR ? source_error_code() : 0;
    r->amp_unmuted = amp_is_unmuted();
    r->dsp_running = dsp_running();
    r->dsp_in = (uint8_t)dsp_input();
    r->spdif_locked = dsp_running() && dsp_spdif_locked() == 1;
    r->dsp_volume = (uint8_t)dsp_volume_index();
    r->aux_jack = inputs()->aux_inserted;
    r->usb_vbus = inputs()->usb_vbus;
    r->led = led_code();
    ir_last(&r->ir_addr, &r->ir_cmd, &r->ir_toggle, &r->ir_ms, &r->ir_frames);
    r->usb_streaming = g_uac.streaming;
    r->usb_rate = g_uac.rate;
    r->usb_mute = g_uac.mute;
    r->usb_packets = udi_audio_packets();
    r->hid = hid_cc_ready();
    usbaudio_stats_t u;
    usbaudio_stats(&u);
    r->usb_step = u.host_step;
    r->engine = u.running;
    r->clock_ok = u.clock_ok;
    r->fill = u.fill;
    r->primed = u.primed;
    r->underruns = u.under;
    r->overruns = u.over;
    r->dropped = u.dropped;
    r->play_fps = u.play_fps;
    r->host_fps = u.host_fps;
    r->ratio = u.ratio;
    r->nominal = u.nominal;
    r->net = wifi_net_connected();
    r->play = (uint8_t)wifi_play_state();
    int st = wifi_pending_volume();
    r->pending_volume = st < 0 ? 255 : (uint8_t)st;
    r->modcon_overruns = modcon_overruns();
    r->amp_on_s = amp_on_s();
    r->amp_run_s = amp_run_s();
    r->heat_cap = protect_heat_cap();
    r->usb_audible = usbaudio_sounding();
    uint32_t sil = source_silent_ms(uptime) / 1000;
    r->silent_s = sil > 0xFFFF ? 0xFFFF : (uint16_t)sil;
    int32_t lv = source_level();
    r->level = lv < 0 ? 0 : (uint32_t)lv;
    r->level_floor = source_level_floor();
}

/* ---- the requests ---- */
static union {
    zd1h_peek_t peek;
    zd1h_poke_t poke;
    zd1h_module_t module;
    zd1h_mod_read_t mrd;
    zd1h_mod_write_t mwr;
    zd1h_get_setting_t gset;
    zd1h_set_setting_t sset;
    zd1h_amp_t amp;
    zd1h_debug_t debug;
    zd1h_lock_t lock;
    zd1h_key_t key;
    zd1h_select_t select;
    zd1h_volume_t volume;
    zd1h_xm_frame_t xm;
    zd1h_pipe_t pipe;
    zd1h_boot_t boot;
#ifdef ZD1_EMU
    zd1h_pipe_data_t pdata;
#endif
    zd1h_update_t update;
    zd1h_prefs_t prefs;
    zd1h_mod_record_t rec;
    zd1h_console_t console;
    zd1h_console_write_t cwrite;
} q; /* the request, decoded */
static union {
    zd1h_hello_reply_t hello;
    zd1h_info_reply_t info;
    zd1h_diag_reply_t diag;
    zd1h_data_t data;
    zd1h_ok_t ok;
    zd1h_xm_reply_t xm;
    zd1h_pipe_reply_t pipe;
    zd1h_boot_reply_t boot;
    zd1h_update_reply_t update;
    zd1h_prefs_reply_t prefs;
    zd1h_console_reply_t console;
} r; /* its reply */
static void ok(void) { reply(&ZD1H_OK, &r.ok); }

/* a gated request may run: the gate open, or unlocked (which it keeps unlocked for GATE_MS more) */
static bool gate_pass(void) {
    uint16_t secs;
    uint8_t g = hostif_gate(&secs);
    if (g == ZD1H_GATE_LOCKED) {
        error(ZD1H_ERR_LOCKED, "press ON/OFF on the speaker to allow it (Unlock)");
        return false;
    }
    if (g == ZD1H_GATE_UNLOCKED)
        unlock_until = uptime + GATE_MS;
    return true;
}

static void req_unlock(void) {
    uint16_t secs;
    if (hostif_gate(&secs) != ZD1H_GATE_LOCKED) {
        ok();
        return;
    }
    confirmed = false;
    confirm_until = uptime + CONFIRM_MS;
    confirm_wait = true; /* hostif_poll finishes it */
    led_flash(FLASH_PURPLE, CONFIRM_MS);
    log_puts("[gate] press ON/OFF on the speaker within 30 s to allow the computer to change it\n");
}

static void req_lock(void) {
    if (q.lock.open)
        g_settings.flags |= SETTINGS_GATE_OPEN;
    else {
        g_settings.flags &= ~SETTINGS_GATE_OPEN;
        unlocked = false;
    }
    settings_changed();
    log_puts(q.lock.open ? "[gate] open for good (EEPROM)\n" : "[gate] locked\n");
    ok();
}

static void req_peek(void) {
    if (q.peek.len > DATA_MAX) {
        error(ZD1H_ERR_ARG, "at most " ASTRINGZ(DATA_MAX) " bytes");
        return;
    }
    if (q.peek.words && (q.peek.addr | q.peek.len) & 3) {
        error(ZD1H_ERR_ARG, "words: addr and len multiples of 4");
        return;
    }
    if (q.peek.words) /* big-endian, as in memory */
        for (uint16_t i = 0; i < q.peek.len; i += 4) {
            uint32_t w = ((volatile uint32_t *)q.peek.addr)[i / 4];
            for (int k = 0; k < 4; k++)
                r.data.data[i + k] = (uint8_t)(w >> (24 - 8 * k));
        }
    else
        for (uint16_t i = 0; i < q.peek.len; i++)
            r.data.data[i] = ((volatile uint8_t *)q.peek.addr)[i];
    r.data.data_len = q.peek.len;
    reply(&ZD1H_DATA, &r.data);
}

/* Poke keeps off the bus matrix and the flash controller (0xFFFE1000..0xFFFE17FF): through them a write could remap
 * memory or erase and program the flash, the bootloader and the fuses included (writes to the flash's own addresses
 * only fill its page buffer) */
static bool poke_refused(uint32_t addr, uint32_t len) {
    uint64_t end = (uint64_t)addr + len;
    return addr < AVR32_FLASHC_ADDRESS + 0x400u && end > AVR32_HMATRIX_ADDRESS;
}
static void req_poke(void) {
    if (poke_refused(q.poke.addr, q.poke.data_len)) {
        error(ZD1H_ERR_ARG, "not the flash controller or the bus matrix");
        return;
    }
    if (q.poke.words && (q.poke.addr | q.poke.data_len) & 3) {
        error(ZD1H_ERR_ARG, "words: addr and length multiples of 4");
        return;
    }
    if (q.poke.words)
        for (uint16_t i = 0; i < q.poke.data_len; i += 4) {
            const uint8_t *d = &q.poke.data[i];
            ((volatile uint32_t *)q.poke.addr)[i / 4] = (uint32_t)d[0] << 24 | (uint32_t)d[1] << 16 | d[2] << 8 | d[3];
        }
    else
        for (uint16_t i = 0; i < q.poke.data_len; i++)
            ((volatile uint8_t *)q.poke.addr)[i] = q.poke.data[i];
    ok();
}

/* the module's registers, for the host: false (an error replied) unless it is up */
static bool module_up(void) {
    if (dm870_is_up())
        return true;
    error(ZD1H_ERR_MODULE, "the module is not up");
    return false;
}
static void req_mod_read(void) {
    if (!module_up())
        return;
    uint16_t v = 0, len = 0;
    bool ok = q.mrd.v ? dm870_reg_read_v(q.mrd.reg, r.data.data, DATA_MAX, &len) : dm870_reg_read(q.mrd.reg, &v);
    if (!ok) {
        error(ZD1H_ERR_MODULE, "no reply from the module");
        return;
    }
    if (!q.mrd.v) {
        r.data.data[0] = (uint8_t)(v >> 8);
        r.data.data[1] = (uint8_t)v;
        len = 2;
    }
    r.data.data_len = len < DATA_MAX ? len : DATA_MAX;
    reply(&ZD1H_DATA, &r.data);
}
static void req_mod_write(void) {
    if (!module_up())
        return;
    if (q.mwr.v)
        dm870_reg_write_v(q.mwr.reg, q.mwr.data, q.mwr.data_len);
    else if (q.mwr.data_len == 2)
        dm870_reg_write(q.mwr.reg, q.mwr.data[0], q.mwr.data[1]);
    else {
        error(ZD1H_ERR_ARG, "an F write takes 2 bytes");
        return;
    }
    ok();
}

/* a query on the module's configuration channel answered (or not): a setting's value, a record's piece */
static bool query_is_rec;
static uint8_t query_rec;
static uint16_t query_off;
static void query_reply(int ok) {
    static union {
        zd1h_setting_reply_t set;
        zd1h_record_reply_t rec;
    } a;
    if (!ok)
        error(ZD1H_ERR_MODULE, "no answer from the module");
    else if (query_is_rec) {
        dm870_record_read(query_rec, query_off, a.rec.data, sizeof a.rec.data, &a.rec.total, &a.rec.data_len);
        reply(&ZD1H_RECORD_REPLY, &a.rec);
    } else {
        dm870_setting_value(a.set.value, sizeof a.set.value);
        reply(&ZD1H_SETTING_REPLY, &a.set);
    }
}
static bool module_ours(void) {
    if (dm870_ours())
        return true;
    error(ZD1H_ERR_MODULE, "the module doesn't run our OS");
    return false;
}
static void req_setting(bool set) {
    if (!module_ours())
        return;
    bool begun = dm870_setting_begin(set ? q.sset.key : q.gset.key, set ? q.sset.value : 0);
    if (set)
        memset(&q, 0, sizeof q); /* a Wi-Fi key doesn't stay in RAM */
    if (!begun) {
        error(ZD1H_ERR_MODULE, "no answer from the module");
        return;
    }
    query_is_rec = false;
    query_wait = true; /* hostif_poll answers it */
}
static void req_record(void) {
    if (!module_ours())
        return;
    int st = dm870_record_begin(q.rec.record, q.rec.offset);
    query_is_rec = true;
    query_rec = q.rec.record;
    query_off = q.rec.offset;
    if (st >= 0)
        query_reply(st);
    else
        query_wait = true; /* fetching it: hostif_poll answers */
}

static void prefs_fill(zd1h_prefs_reply_t *p) {
    p->auto_switch = !(g_settings.flags & SETTINGS_NO_AUTO_SWITCH);
    p->amp_auto = !(g_settings.flags & SETTINGS_AMP_LOCKED);
    p->amp_idle_s = (uint16_t)(g_settings.amp_idle_min * 60u);
    p->volume_cap = g_settings.vol_cap;
}
static void req_prefs(void) {
    unsigned idle = (q.prefs.amp_idle_s + 30u) / 60u; /* whole minutes, within the limits the amp's heat sets */
    if (q.prefs.change & ZD1H_PREFS_CHANGE_AMP_IDLE_S &&
        (idle < SETTINGS_AMP_IDLE_MIN || idle > SETTINGS_AMP_IDLE_MAX)) {
        error(ZD1H_ERR_ARG, "amp idle: 1 to 30 min");
        return;
    }
    if (q.prefs.change & ZD1H_PREFS_CHANGE_VOLUME_CAP && (q.prefs.volume_cap < 1 || q.prefs.volume_cap > VOL_MAX)) {
        error(ZD1H_ERR_ARG, "volume cap: 1 to " ASTRINGZ(VOL_MAX));
        return;
    }
    /* every field checked: now applied */
    uint8_t f = g_settings.flags;
    if (q.prefs.change & ZD1H_PREFS_CHANGE_AUTO_SWITCH)
        f = q.prefs.auto_switch ? f & ~SETTINGS_NO_AUTO_SWITCH : f | SETTINGS_NO_AUTO_SWITCH;
    if (q.prefs.change & ZD1H_PREFS_CHANGE_AMP_IDLE_S && idle != g_settings.amp_idle_min) {
        g_settings.amp_idle_min = (uint8_t)idle;
        settings_changed();
    }
    /* the volume cap: the highest step the computer and the network may set */
    if (q.prefs.change & ZD1H_PREFS_CHANGE_VOLUME_CAP && q.prefs.volume_cap != g_settings.vol_cap) {
        g_settings.vol_cap = q.prefs.volume_cap;
        settings_changed();
        log_puts("[src] volume cap ");
        log_dec(g_settings.vol_cap);
        log_putc('\n');
        if (g_settings.vol_step > g_settings.vol_cap)
            source_volume_set(g_settings.vol_cap); /* down to it now */
        else
            wifi_local_volume(); /* the module maps its senders onto the new range */
    }
    if (f != g_settings.flags) {
        g_settings.flags = f;
        settings_changed();
    }
    prefs_fill(&r.prefs);
    reply(&ZD1H_PREFS_REPLY, &r.prefs);
}

static void req_xm(void) {
    static uint8_t frame[ZD1_WIRE_V_MAX]; /* an XModem-1K frame: one V write */
    if (!module_up())
        return;
    uint32_t end = (uint32_t)q.xm.offset + q.xm.data_len;
    if (end > sizeof frame) {
        error(ZD1H_ERR_ARG, "past the frame buffer");
        return;
    }
    memcpy(frame + q.xm.offset, q.xm.data, q.xm.data_len);
    r.xm.wire_us = r.xm.wait_us = r.xm.read_us = 0;
    if (q.xm.send && end)
        dm870_xm_send(frame, (uint16_t)end, q.xm.wait_ms, &r.xm.wire_us, &r.xm.wait_us, &r.xm.read_us);
    r.xm.answers_len = (uint16_t)dm870_xm_take(r.xm.answers, sizeof r.xm.answers);
    reply(&ZD1H_XM_REPLY, &r.xm);
}

static void req_pipe(void) {
    if (q.pipe.sink != ZD1H_SINK_DISCARD && q.pipe.sink != pipe_sink_now()) {
        error(ZD1H_ERR_ARG, "DISCARD (MODULE is Update's)");
        return;
    }
    pipe_request(q.pipe.sink, q.pipe.reset, update_busy(), &r.pipe);
    reply(&ZD1H_PIPE_REPLY, &r.pipe);
}

static void req_boot(bool set) {
    if (set) {
        if (!modboot_set(q.boot.op, q.boot.slot)) {
            error(ZD1H_ERR_ARG, "TRY: A, B or RESCUE; GOOD: A or B");
            return;
        }
        if (q.boot.restart)
            dm870_op(ZD1H_MOD_OP_RESTART, 0);
    }
    modboot_state(&r.boot.good, &r.boot.next, &r.boot.last, &r.boot.trial, &r.boot.failed);
    reply(&ZD1H_BOOT_REPLY, &r.boot);
}

static void req_update(bool set) {
    const char *why = 0;
    if (set) {
        if (q.update.op == ZD1H_UPDATE_OP_BEGIN)
            why = update_begin(&q.update);
        else if (q.update.op == ZD1H_UPDATE_OP_ABORT)
            update_abort("aborted by the host");
        else
            why = "op: BEGIN or ABORT";
    }
    if (why) {
        error(update_busy() ? ZD1H_ERR_BUSY : ZD1H_ERR_ARG, why);
        return;
    }
    update_state(&r.update);
    reply(&ZD1H_UPDATE_REPLY, &r.update);
}

static void req_key(void) {
    static const uint8_t keys[] = {KEY_ONOFF_SHORT, KEY_ONOFF_HOLD, KEY_STANDBY, KEY_VOLUP,
                                   KEY_VOLDN,       KEY_PLAYPAUSE,  KEY_NEXT,    KEY_PREV}; /* by ZD1H_KEY_* */
    if (q.key.key >= sizeof keys) {
        error(ZD1H_ERR_ARG, "unknown key");
        return;
    }
    source_key(keys[q.key.key]);
    ok();
}

static void req_select(void) {
    uint8_t s = q.select.source;
    if (s >= SRC_N)
        error(ZD1H_ERR_ARG, "unknown source");
    else if (s != SRC_NONE && source_state() == SYS_ERROR)
        error(ZD1H_ERR_ARG, "a protection tripped: ON/OFF on the speaker (or standby) clears it first");
    else if (s == SRC_NONE) {
        source_standby();
        ok();
    } else if (s == SRC_WIFI && !dm870_usable())
        error(ZD1H_ERR_MODULE, dm870_why_unusable());
    else if (s == SRC_WIFI && q.select.player && !source_select_player(q.select.player))
        error(ZD1H_ERR_ARG, "no sender connected to that receiver");
    else if (!(s == SRC_WIFI && q.select.player) && !source_select((source_t)s))
        error(ZD1H_ERR_ARG, s == SRC_IPOD ? "no dock support" : "not plugged in");
    else
        ok();
}

static void req_volume(void) {
    if (q.volume.volume > VOL_MAX || q.volume.bass > BASS_MAX) {
        error(ZD1H_ERR_ARG, "volume 0.." ASTRINGZ(VOL_MAX) ", bass 0.." ASTRINGZ(BASS_MAX));
        return;
    }
    source_volume_set(q.volume.volume);
    source_bass_set(q.volume.bass);
    ok();
}

/* the request's fields into q; false (an Error replied) if they don't decode */
static bool decoded(const zd1h_msg_t *m, const uint8_t *b, uint16_t n) {
    if (zd1h_decode(m, b, n, &q) >= 0)
        return true;
    error(ZD1H_ERR_MALFORMED, "the fields don't decode");
    return false;
}

/* one request: past the gate, decoded, run */
static void run(const uint8_t *b, uint16_t n) {
    uint8_t id = b[0];
    if (zd1h_gated(id) && !gate_pass())
        return;
    switch (id) {
    case ZD1H_ID_UNLOCK:
        if (decoded(&ZD1H_UNLOCK, b, n))
            req_unlock();
        return;
    case ZD1H_ID_LOCK:
        if (decoded(&ZD1H_LOCK, b, n))
            req_lock();
        return;
    case ZD1H_ID_HELLO:
        if (decoded(&ZD1H_HELLO, b, n)) {
            hello_fill(&r.hello);
            reply(&ZD1H_HELLO_REPLY, &r.hello);
        }
        return;
    case ZD1H_ID_INFO:
        if (decoded(&ZD1H_INFO, b, n)) {
            info_fill(&r.info);
            reply(&ZD1H_INFO_REPLY, &r.info);
        }
        return;
    case ZD1H_ID_DIAG:
        if (decoded(&ZD1H_DIAG, b, n)) {
            diag_fill(&r.diag);
            reply(&ZD1H_DIAG_REPLY, &r.diag);
        }
        return;
    case ZD1H_ID_PEEK:
        if (decoded(&ZD1H_PEEK, b, n))
            req_peek();
        return;
    case ZD1H_ID_POKE:
        if (decoded(&ZD1H_POKE, b, n))
            req_poke();
        return;
    case ZD1H_ID_DFU:
    case ZD1H_ID_REBOOT:
        if (!decoded(id == ZD1H_ID_DFU ? &ZD1H_DFU : &ZD1H_REBOOT, b, n))
            return;
        ok();
        wait_taken();
        if (id == ZD1H_ID_DFU)
            selfdfu_enter();
        else
            selfdfu_reboot();
        return;
    case ZD1H_ID_MODULE:
        if (!decoded(&ZD1H_MODULE, b, n))
            return;
        if (q.module.op > ZD1H_MOD_OP_ESC) {
            error(ZD1H_ERR_ARG, "unknown op");
            return;
        }
        dm870_op(q.module.op, q.module.timeout_s ? q.module.timeout_s : 120);
        module_wait = true; /* hostif_poll answers it */
        return;
    case ZD1H_ID_MOD_READ:
        if (decoded(&ZD1H_MOD_READ, b, n))
            req_mod_read();
        return;
    case ZD1H_ID_MOD_WRITE:
        if (decoded(&ZD1H_MOD_WRITE, b, n))
            req_mod_write();
        return;
    case ZD1H_ID_GET_SETTING:
        if (decoded(&ZD1H_GET_SETTING, b, n))
            req_setting(false);
        return;
    case ZD1H_ID_SET_SETTING:
        if (decoded(&ZD1H_SET_SETTING, b, n))
            req_setting(true);
        return;
    case ZD1H_ID_MOD_RECORD:
        if (decoded(&ZD1H_MOD_RECORD, b, n))
            req_record();
        return;
    case ZD1H_ID_AMP:
        if (decoded(&ZD1H_AMP, b, n)) {
            source_amp_allow(q.amp.allow, q.amp.keep);
            ok();
        }
        return;
    case ZD1H_ID_PREFS:
        if (decoded(&ZD1H_PREFS, b, n))
            req_prefs();
        return;
    case ZD1H_ID_DEBUG:
        if (!decoded(&ZD1H_DEBUG, b, n))
            return;
        if (q.debug.mode > ZD1H_DEBUG_OFF) {
            error(ZD1H_ERR_ARG, "mode: DEFAULT, ON or OFF");
            return;
        }
        dm870_set_debug(q.debug.mode);
        ok();
        return;
    case ZD1H_ID_XM_FRAME:
        if (decoded(&ZD1H_XM_FRAME, b, n))
            req_xm();
        return;
    case ZD1H_ID_PIPE:
        if (decoded(&ZD1H_PIPE, b, n))
            req_pipe();
        return;
    case ZD1H_ID_BOOT_STATE:
        if (decoded(&ZD1H_BOOT_STATE, b, n))
            req_boot(false);
        return;
    case ZD1H_ID_BOOT:
        if (decoded(&ZD1H_BOOT, b, n))
            req_boot(true);
        return;
#ifdef ZD1_EMU
    case ZD1H_ID_PIPE_DATA: /* the emulator's USB carries EP0 only */
        if (!decoded(&ZD1H_PIPE_DATA, b, n))
            return;
        if (pipe_put(q.pdata.data, q.pdata.data_len))
            ok();
        else
            error(ZD1H_ERR_BUSY, "both buffers full");
        return;
#endif
    case ZD1H_ID_UPDATE:
        if (decoded(&ZD1H_UPDATE, b, n))
            req_update(true);
        return;
    case ZD1H_ID_UPDATE_STATUS:
        if (decoded(&ZD1H_UPDATE_STATUS, b, n))
            req_update(false);
        return;
    case ZD1H_ID_KEY:
        if (decoded(&ZD1H_KEY, b, n))
            req_key();
        return;
    case ZD1H_ID_SELECT:
        if (decoded(&ZD1H_SELECT, b, n))
            req_select();
        return;
    case ZD1H_ID_VOLUME:
        if (decoded(&ZD1H_VOLUME, b, n))
            req_volume();
        return;
    case ZD1H_ID_CONSOLE:
        if (!decoded(&ZD1H_CONSOLE, b, n))
            return;
        if (q.console.open != modcon_host())
            log_puts(q.console.open ? "[con] the module's console: open for the host\n"
                                    : "[con] the module's console: closed\n");
        modcon_host_open(q.console.open);
        r.console.open = modcon_host();
        r.console.taken = 0;
        r.console.lost = modcon_host_lost();
        reply(&ZD1H_CONSOLE_REPLY, &r.console);
        return;
    case ZD1H_ID_CONSOLE_WRITE:
        if (!decoded(&ZD1H_CONSOLE_WRITE, b, n))
            return;
        if (!modcon_host()) {
            error(ZD1H_ERR_ARG, "the module's console isn't open (Console)");
            return;
        }
        r.console.open = true;
        r.console.taken = (uint16_t)modcon_host_put(q.cwrite.data, q.cwrite.data_len);
        r.console.lost = modcon_host_lost();
        reply(&ZD1H_CONSOLE_REPLY, &r.console);
        return;
    default:
        error(ZD1H_ERR_UNKNOWN, "unknown request");
        return;
    }
}

void hostif_poll(uint32_t now) {
    uptime = now;
    pipe_poll();
    if (polled) {
        polled = false;
        last_poll = now;
        log_reader(true);
    } else if (now - last_poll > 1000)
        log_reader(false); /* gone: a full log doesn't wait for it */
    if (hello_closed) {
        hello_closed = false;
        log_puts("[con] the module's console: closed (a new host)\n");
    }
    if (modcon_host() && now - last_poll > 2000) {
        modcon_host_open(false); /* nobody reads it */
        log_puts("[con] the module's console: closed (the host stopped polling)\n");
    }
    if (confirm_wait) { /* an Unlock: the press, or its time runs out (the request stays in) */
        static const zd1h_ok_t ok;
        if (confirmed) {
            confirm_wait = false;
            unlocked = true;
            unlock_until = now + GATE_MS;
            led_flash(FLASH_GREEN, 500);
            log_puts("[gate] unlocked on the speaker\n");
            reply(&ZD1H_OK, &ok);
        } else if (tick_after(now, confirm_until)) {
            confirm_wait = false;
            led_flash(FLASH_RED, 500);
            log_puts("[gate] not confirmed: still locked\n");
            error(ZD1H_ERR_DENIED, "nobody pressed ON/OFF on the speaker within 30 s");
        } else
            return;
        req_full = false;
        return;
    }
    if (module_wait) { /* a Module request: answered when its op ends */
        int ok = dm870_op_result();
        if (ok < 0)
            return;
        zd1h_module_reply_t m = {.ok = ok, .state = dm870_state(), .status = dm870_last_status()};
        module_wait = false;
        reply(&ZD1H_MODULE_REPLY, &m);
        req_full = false;
        return;
    }
    if (query_wait) { /* a setting or a record: answered when the module does */
        int ok = dm870_query_result();
        if (ok < 0)
            return;
        query_wait = false;
        query_reply(ok);
        req_full = false;
        return;
    }
    if (!req_full)
        return;
    cur_seq = req_seq;
    run(req, req_len);
    if (!confirm_wait && !module_wait && !query_wait)
        req_full = false;
}
