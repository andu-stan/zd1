#include <sysclk.h>
#include <gpio.h>
#include <pdca.h>
#include <ssc_i2s.h>
#include "log.h"
#include "tick.h"
#include "cs2000.h"
#include "source.h"
#include "udi_audio.h"
#include "udi_hid_cc.h"
#include "usbaudio.h"
#include "hostvol.h"
#include "settings.h"

#define PDCA_CH      0
#define PDCA_LVL     3  /* stock: PDCA ISR at level 3 (FUN_800232d8) */
#define BLK          64 /* frames per PDCA block (1.3 ms at 48 kHz) */
#define NBLK         16
#define RING         (BLK * NBLK) /* 1024 frames, power of two */
#define INFLIGHT     (2 * BLK)    /* current + reload block may be ring blocks the PDCA is still reading */
#define TARGET       (8 * BLK)    /* fill target and priming threshold: 512 frames = 10.7 ms at 48 kHz */
#define TRIM_MS      1000
#define TRIM_MAX     1000  /* ppm */
#define SOUND_MIN    16    /* a sample over this (-66 dBFS) is sound; hosts send digital zeros when idle */
#define SOUND_GAP_MS 500   /* sounding: a loud packet within this */
#define SOUND_END_MS 10000 /* a session ends after this much silence (longer than a gap between tracks) */

/* stock pin map 0x80022fb0: PA18 RX_CLK, PA15 TX_CLK, PA16 TX_DATA, PA14 TX_FS, all function A.
 * (PA17 RX_DATA = module reset and PA19 RX_FS = USB mux on this board: never mapped.) */
static const gpio_map_t ssc_pins = {
    {AVR32_SSC_RX_CLOCK_0_PIN, AVR32_SSC_RX_CLOCK_0_FUNCTION},
    {AVR32_SSC_TX_CLOCK_0_PIN, AVR32_SSC_TX_CLOCK_0_FUNCTION},
    {AVR32_SSC_TX_DATA_0_PIN, AVR32_SSC_TX_DATA_0_FUNCTION},
    {AVR32_SSC_TX_FRAME_SYNC_0_PIN, AVR32_SSC_TX_FRAME_SYNC_0_FUNCTION},
};

/* Ring of native (big-endian) 16-bit L,R frames: the PDCA reads half-words and the SSC sends MSB first, USB PCM is
 * little-endian, so samples are byte-swapped on the way in. Single producer (the USB interrupt), single consumer
 * (the PDCA interrupt); both counters only grow. */
static int16_t ring[RING * 2];
static int16_t silence[BLK * 2];
static volatile uint32_t wr_f, rd_f;
static volatile bool primed;
static volatile uint32_t n_blocks, n_under, n_over, n_drop, n_rx_frames, fill_acc, fill_n;
static volatile uint32_t n_loud; /* packets with sound in them */

static bool running, irq_done;
static uint32_t cur_fs, nominal, ratio;
static int32_t integ16; /* rate-trim integrator, ppm * 16 */
static int32_t trim16;  /* last applied trim, ppm * 16 */
static uint32_t t_trim, last_blocks, last_rx, meas_play, meas_host;
static bool host_vol_seen;
static bool clock_ok; /* frame sync toggled at start = bit clock present on RX_CLK */

ISR(pdca_isr, AVR32_PDCA_IRQ_0 / 32, PDCA_LVL) {
    uint32_t fill = wr_f - rd_f;
    n_blocks++;
    if (!primed && fill >= TARGET)
        primed = true;
    if (primed && fill >= BLK) {
        pdca_reload_channel(PDCA_CH, &ring[(rd_f % RING) * 2], BLK * 2);
        rd_f += BLK;
        fill_acc += fill;
        fill_n++;
    } else {
        if (primed) {
            primed = false;
            n_under++;
        }
        pdca_reload_channel(PDCA_CH, silence, BLK * 2);
    }
}

static uint32_t ring_free(void) { return RING - INFLIGHT - (wr_f - rd_f); }

void usbaudio_rx(const uint8_t *b, uint32_t bytes) {
    uint32_t frames = bytes / 4;
    n_rx_frames += frames;
    for (uint32_t k = 0; k < frames * 2; k++) { /* LE 16-bit: |s| > SOUND_MIN, stops at the first */
        int16_t v = (int16_t)(b[2 * k] | b[2 * k + 1] << 8);
        if (v > SOUND_MIN || v < -SOUND_MIN) {
            n_loud++;
            break;
        }
    }
    if (!running) {
        n_drop += frames;
        return;
    }
    if (frames > ring_free()) {
        n_over++;
        return;
    }
    uint32_t w = wr_f;
    for (uint32_t i = 0; i < frames; i++, b += 4) {
        int16_t l = (int16_t)(b[0] | b[1] << 8), r = (int16_t)(b[2] | b[3] << 8);
        int16_t *s = &ring[(w++ % RING) * 2];
        s[0] = l;
        s[1] = r;
    }
    wr_f = w;
}

static bool valid_fs(uint32_t fs) { return fs == 48000 || fs == 44100 || fs == 32000; }

static void engine_stop(void) {
    if (!running)
        return;
    pdca_disable_interrupt_reload_counter_zero(PDCA_CH);
    pdca_disable(PDCA_CH);
    ssc_i2s_reset(&AVR32_SSC);
    gpio_enable_gpio(ssc_pins, sizeof ssc_pins / sizeof ssc_pins[0]);
    running = false;
    cs2000_set_rate(44100); /* back to the boot clock for the other sources */
}

static void engine_start(uint32_t fs) {
    engine_stop();
    nominal = ratio = cs2000_ratio_for(fs);
    cs2000_set_ratio(ratio);
    integ16 = trim16 = 0;
    irqflags_t f = cpu_irq_save();
    wr_f = rd_f = 0;
    primed = false;
    fill_acc = fill_n = 0;
    cpu_irq_restore(f);

    ssc_i2s_init(&AVR32_SSC, fs, 16, 32, SSC_I2S_MODE_STEREO_OUT_EXT_CLK, sysclk_get_pba_hz());
    gpio_enable_module(ssc_pins, sizeof ssc_pins / sizeof ssc_pins[0]);
    pdca_channel_options_t o = {
        .addr = silence,
        .size = BLK * 2,
        .r_addr = silence,
        .r_size = BLK * 2,
        .pid = AVR32_PDCA_PID_SSC_TX,
        .transfer_size = PDCA_TRANSFER_SIZE_HALF_WORD,
    };
    pdca_init_channel(PDCA_CH, &o);
    if (!irq_done) {
        irq_register_handler(pdca_isr, AVR32_PDCA_IRQ_0 + PDCA_CH, PDCA_LVL);
        irq_done = true;
    }

    /* Start the DMA right after a frame-sync rising edge (= start of the right slot), so the first sample, left,
     * goes out in the next left slot (stock FUN_800230e8 waits on PA14 the same way). Bounded: without a bit clock on
     * RX_CLK the frame sync never moves; start anyway, status shows it. */
    uint32_t end = tick_ms() + 3; /* 2-3 ms: a frame is 31 us at 32 kHz */
    clock_ok = false;
    while (!tick_after(tick_ms(), end) && gpio_get_pin_value(AVR32_SSC_TX_FRAME_SYNC_0_PIN)) {
    }
    while (!tick_after(tick_ms(), end)) {
        if (gpio_get_pin_value(AVR32_SSC_TX_FRAME_SYNC_0_PIN)) {
            clock_ok = true;
            break;
        }
    }
    pdca_enable_interrupt_reload_counter_zero(PDCA_CH);
    pdca_enable(PDCA_CH);

    running = true;
    cur_fs = fs;
    last_blocks = n_blocks;
    last_rx = n_rx_frames;
    meas_play = meas_host = 0;
}

/* Adaptive sink: fill above target = host faster than us -> raise MCLK. PI on the average fill over 1 s,
 * Kp 2 ppm/frame, Ki 0.05 ppm/frame/s (~critically damped at 48 kHz), clamped to +-1000 ppm. */
static void rate_trim(void) {
    irqflags_t f = cpu_irq_save();
    uint32_t acc = fill_acc, n = fill_n;
    fill_acc = fill_n = 0;
    cpu_irq_restore(f);
    if (!primed || n == 0)
        return;
    int32_t err = (int32_t)(acc / n) - TARGET;
    integ16 += err * 4 / 5;
    if (integ16 > TRIM_MAX * 16)
        integ16 = TRIM_MAX * 16;
    if (integ16 < -TRIM_MAX * 16)
        integ16 = -TRIM_MAX * 16;
    int32_t t = err * 32 + integ16;
    if (t > TRIM_MAX * 16)
        t = TRIM_MAX * 16;
    if (t < -TRIM_MAX * 16)
        t = -TRIM_MAX * 16;
    trim16 = t;
    uint32_t r = (uint32_t)((int64_t)nominal + (int64_t)nominal * t / 16000000);
    if (r != ratio) {
        ratio = r;
        cs2000_set_ratio(r);
    }
}

static uint8_t host_step(void) { /* FU volume (1/256 dB) -> the step, the host's range onto 1..the cap (hostvol.h) */
    return hostvol_step(((int32_t)g_uac.volume + 128) >> 8, g_settings.vol_cap);
}

/* The computer picking this speaker as its output: macOS opens the stream at once, sends silence and closes it ~2 s
 * later; switching away does nothing on USB, and a system sound opens it with sound [HW 2026-10-27]. A stream that
 * opened and closed silent after PICK_MIN_MS..PICK_MAX_MS is a pick. */
#define PICK_MIN_MS 800
#define PICK_MAX_MS 5000
static bool was_streaming, picked, opened;
static uint32_t t_open, open_loud_n;
static bool session, ever_loud;
static uint32_t t_session, t_loud, t_run, last_loud; /* t_run: the current run of sound (no gap > 500 ms) */

static void sound_poll(uint32_t now) {
    uint32_t n = n_loud;
    if (g_uac.streaming != was_streaming) {
        was_streaming = g_uac.streaming;
        if (was_streaming) {
            t_open = now;
            open_loud_n = n;
            opened = true;
        } else if (n == open_loud_n && now - t_open >= PICK_MIN_MS && now - t_open <= PICK_MAX_MS) {
            picked = true;
            log_puts("[usb] the computer picked the speaker as its output (a silent stream for ");
            log_dec(now - t_open);
            log_puts(" ms)\n");
        }
    }
    if (n != last_loud) {
        if (!ever_loud || now - t_loud > SOUND_GAP_MS)
            t_run = now;
        last_loud = n;
        t_loud = now;
        ever_loud = true;
        if (!session) {
            session = true;
            t_session = now;
        }
    } else if (session && (!g_uac.streaming || now - t_loud > SOUND_END_MS))
        session = false;
}
bool usbaudio_session(uint32_t *since) {
    if (since)
        *since = t_session;
    return session;
}
bool usbaudio_sounding(void) { return session && tick_ms() - t_loud < SOUND_GAP_MS; }
uint32_t usbaudio_sounding_ms(uint32_t now) { return usbaudio_sounding() ? now - t_run : 0; }
bool usbaudio_take_pick(void) {
    bool p = picked;
    picked = false;
    return p;
}
bool usbaudio_take_open(void) {
    bool o = opened;
    opened = false;
    return o;
}

/* the host's requests in the log: what choosing this speaker as the output does on USB (mcu/README.md, Intents) */
static void host_log(void) {
    uac_event_t e;
    uint8_t lost;
    while (udi_audio_event(&e, &lost)) {
        if (lost) {
            log_puts("[usb] host: (");
            log_dec(lost);
            log_puts(" requests not logged)\n");
        }
        log_puts("[usb] ");
        switch (e.what) {
        case UAC_EV_ALT:
            log_puts(e.value == 1   ? "host: stream on"
                     : e.value == 0 ? "host: stream off"
                                    : "host: stream interface disabled");
            break;
        case UAC_EV_RATE:
            log_puts("host: rate ");
            log_dec((uint32_t)e.value);
            break;
        case UAC_EV_SET_VOL:
            log_puts("host: volume ");
            if (e.value < 0) {
                log_putc('-');
                e.value = -e.value;
            }
            log_dec((uint32_t)e.value / 256);
            log_puts(" dB");
            break;
        case UAC_EV_SET_MUTE:
            log_puts(e.value ? "host: mute" : "host: unmute");
            break;
        case UAC_EV_GET: {
            static const char *req[] = {"current", "minimum", "maximum", "resolution"};
            unsigned r = ((uint32_t)e.value >> 8) - 0x81;
            log_puts("host: reads the ");
            log_puts((e.value & 0xFF) == 1 ? "mute" : "volume");
            log_putc(' ');
            log_puts(r < 4 ? req[r] : "?");
            break;
        }
        case UAC_EV_SUSPEND:
            log_puts("bus suspended");
            break;
        case UAC_EV_RESUME:
            log_puts("bus resumed");
            break;
        case UAC_EV_STATUS:
            log_puts(e.value ? "host: took the status word" : "status word: the volume changed here");
            break;
        case UAC_EV_REFUSED:
            log_puts("host: request 0x");
            log_hex8((uint8_t)((uint32_t)e.value >> 24));
            log_puts(" to entity ");
            log_dec((uint32_t)e.value >> 16 & 0xFF);
            log_puts(", wValue 0x");
            log_hexn((uint32_t)e.value & 0xFFFF, 4);
            log_puts(": refused");
            break;
        default:
            log_puts("?");
            break;
        }
        log_putc('\n');
    }
}

void usbaudio_poll(uint32_t now) {
    host_log();
    irqflags_t f = cpu_irq_save();
    uint8_t ch = g_uac.changed;
    g_uac.changed = 0;
    cpu_irq_restore(f);
    sound_poll(now);

    /* The host's volume is the master while USB is the source: applied when it changes and when USB becomes the
     * source (macOS sends its saved value at enumeration, possibly while we play AUX). Keys step here and tell the host. */
    static bool was_usb;
    bool usb_src = source_state() == SYS_RUN && source_current() == SRC_USB;
    if (ch & UAC_CHG_VOLUME)
        host_vol_seen = true;
    if (usb_src && host_vol_seen && ((ch & UAC_CHG_VOLUME) || !was_usb))
        source_host_volume(host_step());
    was_usb = usb_src;
    if (ch & UAC_CHG_MUTE)
        source_host_mute(g_uac.mute);

    uint32_t fs = valid_fs(g_uac.rate) ? g_uac.rate : 48000;
    bool want = g_uac.streaming && usb_src;
    if (want && (!running || fs != cur_fs || (ch & UAC_CHG_ALT))) {
        source_ramp_in(); /* muted now, back up in 200 ms: no click at a start or a rate switch */
        engine_start(fs);
    } else if (!want && running)
        engine_stop();

    if (!running)
        return;
    if (tick_after(now, t_trim + TRIM_MS)) {
        t_trim = now;
        uint32_t b = n_blocks, r = n_rx_frames;
        meas_play = (b - last_blocks) * BLK;
        meas_host = r - last_rx; /* frames per second */
        last_blocks = b;
        last_rx = r;
        rate_trim();
    }
}

/* a volume change on the speaker, for the host (GET_CUR, and a status word where there is one); a step the host's
 * volume already reads as (its own change) leaves the host's value as it set it */
void usbaudio_local_volume(uint8_t step) {
    if (step != host_step())
        udi_audio_set_volume((int16_t)(hostvol_db(step, g_settings.vol_cap) * 256));
}

uint8_t usbaudio_volmax(void) { return g_settings.vol_cap; } /* the volume cap (Prefs): the one limit, VOL_MAX = none */

void usbaudio_stats(usbaudio_stats_t *st) {
    irqflags_t f = cpu_irq_save();
    st->fill = (uint16_t)(wr_f - rd_f);
    st->primed = primed;
    st->under = n_under;
    st->over = n_over;
    st->dropped = n_drop;
    cpu_irq_restore(f);
    st->running = running;
    st->clock_ok = clock_ok;
    st->play_fps = meas_play;
    st->host_fps = meas_host;
    st->ratio = ratio;
    st->nominal = nominal;
    st->host_step = host_vol_seen ? host_step() : 0;
}
