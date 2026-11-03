#include <sysclk.h>
#include <gpio.h>
#include <spi.h>
#include <delay.h>
#include <string.h>
#include "zd1_board.h"
#include "log.h"
#include "dm870.h"
#include "tick.h"
#include "wifi.h"
#include "hostif.h"
#include "modboot.h"
#include "modcon.h"
#include "update.h"
#include "util.h"

static bool mod_up;
static bool ready_seen;      /* status 0x2100 / 0x2300 seen since the last power-up */
static bool ours;            /* reg 0x25 says our module OS (not the stock app, not BL2) */
static bool native;          /* ... and reg 0xD5 says it speaks this firmware's protocol */
static uint8_t mod_proto;    /* what reg 0xD5 said (0: nothing, or not ours) */
static uint16_t last_status; /* the last reg 0x11 read */
static char os_version[65], mod_ip[17], mod_name[65];
static uint8_t debug_mode; /* ZD1H_DEBUG_*: RAM only, sent at every ready of our OS once set */
static uint16_t cfg_len;   /* a setting's value, as the module answered its query */
static char cfg_val[129];
typedef enum { BU_IDLE, BU_OFF, BU_PWR, BU_RST, BU_WAIT, BU_READY, BU_BL2 } bu_state_t;
static bu_state_t bu;
static bool hold; /* this power-up ends at the reset release: no SPI (Module HOLD) */
/* The host link's Module POWER / BL2 / HOLD: run on from dm870_poll; the request is answered when it ends
 * (dm870_op_result) */
typedef enum { OP_NONE, OP_POWER, OP_BL2, OP_HOLD, OP_ESC } op_t;
static op_t op;
static int8_t op_result = 1; /* -1 while an op runs */
static uint32_t op_deadline;
static bool esc_on;                       /* ESC: the escape started at this op's reset release */
static uint8_t nbuf[ZD1_WIRE_V_MAX];      /* a V read: the longest the module sends */
static uint8_t state_rec[ZD1_WIRE_V_MAX]; /* the module's State record (reg 0xD6), as it last pushed it */
static uint16_t state_len;
static int16_t rec_num = -1;            /* a record fetched through the configuration channel (zd1/@N) ... */
static uint8_t rec_buf[ZD1_WIRE_V_MAX]; /* ... its bytes, for the host's pieces */
static uint16_t rec_len;
/* a query on the configuration channel (a setting, a record): sent from dm870_poll, its answer taken by drain() */
static enum { QUERY_NONE, QUERY_SEND, QUERY_WAIT } query;
static bool query_rec; /* ... a record's (into rec_buf; else a setting's, into cfg_val) */
#define CONFIG_NAME_MAX 32
static uint8_t query_buf[6 + CONFIG_NAME_MAX]; /* [Q][len] "zd1/" NAME */
static uint16_t query_len;
static uint32_t query_deadline;
static int8_t query_result = 1; /* -1 while one runs */
static void query_end(int8_t ok) {
    query = QUERY_NONE;
    query_result = ok;
    if (!ok && query_rec)
        rec_num = -1;
}

bool dm870_is_up(void) { return mod_up; }
bool dm870_powered(void) { return gpio_get_gpio_pin_output_value(PIN_MOD_PWR); }
bool dm870_ours(void) { return ours && mod_up; }
bool dm870_native(void) { return native && dm870_ours(); }
uint8_t dm870_proto(void) { return mod_proto; }
/* The module can be the speaker's source, or may yet: powered, and not known to speak another protocol (the stock
 * app, our OS with another link protocol). While it boots the answer is yes: it may come up as ours. */
bool dm870_usable(void) { return dm870_powered() && !(ready_seen && !native); }
const char *dm870_why_unusable(void) {
    if (!dm870_powered())
        return "the module is off";
    if (!ours)
        return "the module runs the stock software, not ours: it is no source";
    return "the module's OS speaks another link protocol than this firmware: update one of them";
}
uint16_t dm870_last_status(void) { return last_status; }
const char *dm870_os_version(void) { return os_version; }
const char *dm870_ip(void) { return mod_ip; }
const char *dm870_name(void) { return mod_name; }
uint8_t dm870_debug(void) { return debug_mode; }

/* ---- SPI0, NPCS2 = PA09, mode 3, ~3 MHz ---- */
static void spi0_init(void) {
    static const gpio_map_t pins = {
        {AVR32_SPI0_NPCS_2_0_PIN, AVR32_SPI0_NPCS_2_0_FUNCTION}, /* PA09 */
        {AVR32_SPI0_MISO_0_0_PIN, AVR32_SPI0_MISO_0_0_FUNCTION}, /* PA11 */
        {AVR32_SPI0_MOSI_0_0_PIN, AVR32_SPI0_MOSI_0_0_FUNCTION}, /* PA12 */
        {AVR32_SPI0_SCK_0_0_PIN, AVR32_SPI0_SCK_0_0_FUNCTION},   /* PA13 */
    };
    const spi_options_t opt = {.reg = 2,
                               .baudrate = 3000000,
                               .bits = 8,
                               .spck_delay = 0,
                               .trans_delay = 0,
                               .stay_act = 1,
                               .spi_mode = 3,
                               .modfdis = 1};
    gpio_enable_module(pins, sizeof pins / sizeof pins[0]);
    spi_initMaster(&AVR32_SPI0, &opt);
    spi_selectionMode(&AVR32_SPI0, 0, 0, 0); /* fixed peripheral select */
    spi_setupChipReg(&AVR32_SPI0, &opt, sysclk_get_pba_hz());
    spi_enable(&AVR32_SPI0);
    spi_unselectChip(&AVR32_SPI0, 2); /* every request toggles NPCS2 itself (SPI_PROTOCOL 1.3) */
}
static uint8_t spi0_xfer(uint8_t b) { /* ASF spi_write/spi_read are timeout-bounded */
    uint16_t d = 0xFF;
    if (spi_write(&AVR32_SPI0, b) == SPI_OK)
        spi_read(&AVR32_SPI0, &d);
    return (uint8_t)d;
}

/* ---- power ---- */
/* what the module said about itself, gone with it (a power-off, or it restarted by itself) */
static void forget_module(void) {
    ready_seen = ours = native = false;
    mod_proto = 0;
    last_status = 0;
    os_version[0] = mod_ip[0] = mod_name[0] = 0;
    state_len = rec_len = 0;
    rec_num = -1;
    if (query != QUERY_NONE)
        query_end(0);
}
/* Stock detach / safe power-down (mirrors FUN_8000dcdc): reset first, settle, then cut power.
 * Returns the SPI pins to inputs. Always leaves the module OFF; a running update fails. */
void dm870_detach(void) {
    update_abort("the module was powered off");
    bu = BU_IDLE;
    hold = false;
    modboot_cancel();
    gpio_set_pin_low(PIN_MOD_RST);
    delay_ms(20);
    modcon_power(false);
    gpio_set_pin_low(PIN_MOD_PWR);
    gpio_configure_pin(AVR32_PIN_PA09, GPIO_DIR_INPUT);
    gpio_configure_pin(AVR32_PIN_PA11, GPIO_DIR_INPUT);
    gpio_configure_pin(AVR32_PIN_PA12, GPIO_DIR_INPUT);
    gpio_configure_pin(AVR32_PIN_PA13, GPIO_DIR_INPUT);
    mod_up = false;
    forget_module();
}

/* ---- register protocol (SPI_PROTOCOL 1.2/1.3) ----
 * Stuffing: data 0x27 -> 27 27, data 0xFF -> 27 01; a raw 0xFF on the wire is idle.
 * Write F: CS low, 00 reg b0 b1, CS high. Read: CS low, 01|03 reg 00 00, CS high, 5 ms, CS low, clock 0xFF:
 * skip to the type byte (500 ms), skip to the reg byte (500 ms), then the data, CS high.
 * Every byte is stuffed, the header too: the stock firmware's writer stuffs whole messages and its reader unstuffs
 * every byte (0x8000c050 / 0x8000c154). It only shows for register 0x27 (VersionCNE) and 0xFF. */
static void mod_cs(bool on) {
    if (on)
        spi_selectChip(&AVR32_SPI0, 2);
    else
        spi_unselectChip(&AVR32_SPI0, 2);
}
static void mod_put(uint8_t b) { /* one byte, stuffed (the spec's framing: zd1link_gen) */
    uint8_t o[2], n = zd1_stuff(b, o);
    for (uint8_t i = 0; i < n; i++)
        spi0_xfer(o[i]);
}
/* Idle polling for the reply: 0xFF after 0xFF, unpaced. The B&W DFU bootloader waits 5 ms between polls (up to 255,
 * its read at 0x80002890); the stock app and BL2 answer unpaced as well [HW 2026-10-19]. Every wait is bounded by one
 * deadline for the whole reply, so that the main loop
 * feeds the watchdog (main.c) in time: READ_MS for a read (1 KB is ~4 ms on the wire, and zd1-linkd polls the link at
 * least every tick, 10 ms, without pausing while a reply goes out), ECHO_MS for a write's echo. */
#define READ_MS 300
#define ECHO_MS 300
static int mod_get(uint32_t end) { /* next unstuffed byte (idle skipped), -1 at the deadline (tick_ms) */
    zd1_unstuff_t u = {0};
    while (!tick_after(tick_ms(), end)) {
        uint8_t b;
        int r = zd1_unstuff(&u, spi0_xfer(ZD1_WIRE_IDLE), &b);
        if (r == ZD1_RX_BYTE)
            return b;
    }
    return -1;
}
static bool mod_skip_to(uint8_t want, uint32_t end) { /* skip to an (unstuffed) header byte */
    int b;
    while ((b = mod_get(end)) >= 0)
        if (b == want)
            return true;
    return false;
}
void dm870_reg_write(uint8_t reg, uint8_t b0, uint8_t b1) {
    mod_cs(true);
    spi0_xfer(ZD1_WIRE_WRITE_F);
    mod_put(reg);
    mod_put(b0);
    mod_put(b1);
    mod_cs(false);
}
/* V write: 02 reg lenLo lenHi data (length + data stuffed), as stock sends reg 0x40 / 0x45 (emulator trace). A long
 * one is paced: the module's SPI slave has a 64-byte RX FIFO, and zd1-linkd polls it once per kernel tick (10 ms)
 * while idle, so the rest of a long write overflowed it and the request arrived broken or not at all [HW 2026-10-26:
 * Update BEGIN, 70 bytes]. After the header it waits a tick (zd1-linkd then polls without sleeping until the request
 * is complete), then sends the data 32 bytes at a time: to our OS 15 ms apart, since zd1-linkd is a process
 * that others preempt for a scheduler slice (up to ~20 ms) while the services start, and at 1 ms apart a request
 * still arrived short after boot [HW 2026-11-02: an Update BEGIN half received, zd1-linkd 0b28a16]; to BL2 and the
 * bootloader's relay (bare metal, polling) 1 ms apart. Short writes (the stock app's) go as they always did.
 * To our OS the write is acknowledged: the module echoes every write (02 reg 00 00), read like a reply; without the
 * echo the bytes were lost (the FIFO overran while zd1-linkd was busy: right after its boot, with the services
 * starting, the Update BEGIN arrived short and the half-received request wedged the slave's parser until the
 * link was restarted [HW 2026-10-31]) and the write goes again, up to three times, once the module has dropped the
 * half it got (100 ms of silence): at most ~1.6 s in all (a 170-byte setting). BL2 and the bootloader's relay get no such check. */
static bool write_v_once(uint8_t reg, const uint8_t *d, uint16_t n) {
    bool pace = n > 48;
    mod_cs(true);
    spi0_xfer(ZD1_WIRE_WRITE_V);
    mod_put(reg);
    mod_put((uint8_t)n);
    mod_put((uint8_t)(n >> 8));
    if (pace)
        delay_ms(15);
    for (uint16_t i = 0; i < n; i++) {
        if (pace && i && !(i % 32))
            delay_ms(ours ? 15 : 1);
        mod_put(d[i]);
    }
    mod_cs(false);
    if (!ours)
        return true;
    delay_ms(5);
    mod_cs(true);
    uint32_t end = tick_ms() + ECHO_MS;
    bool echoed = mod_skip_to(ZD1_WIRE_WRITE_V, end) && mod_skip_to(reg, end) && mod_get(end) >= 0 && mod_get(end) >= 0;
    mod_cs(false);
    return echoed;
}
bool dm870_reg_write_v(uint8_t reg, const uint8_t *d, uint16_t n) {
    for (uint8_t t = 1; t <= 3; t++) {
        if (write_v_once(reg, d, n))
            return true;
        log_puts("[mod] V write 0x");
        log_hex8(reg);
        log_puts(" not acknowledged: ");
        if (t == 3) {
            log_puts("given up\n");
            return false;
        }
        log_puts("again\n");
        delay_ms(150); /* the module drops the half it got after 100 ms of silence */
    }
    return false;
}
static void request(uint8_t type, uint8_t reg) {
    mod_cs(true);
    spi0_xfer(type);
    mod_put(reg);
    spi0_xfer(0x00);
    spi0_xfer(0x00);
    mod_cs(false);
    delay_ms(5);
    mod_cs(true);
}
bool dm870_reg_read(uint8_t reg, uint16_t *val) {
    bool ok = false;
    request(ZD1_WIRE_READ_F, reg);
    uint32_t end = tick_ms() + READ_MS;
    if (mod_skip_to(ZD1_WIRE_READ_F, end) && mod_skip_to(reg, end)) {
        int b0 = mod_get(end), b1 = b0 < 0 ? -1 : mod_get(end);
        if (b1 >= 0) {
            *val = (uint16_t)((b0 << 8) | b1);
            ok = true;
        }
    }
    mod_cs(false);
    if (ok && reg == ZD1_REG_STATUS) {
        last_status = *val;
        if (*val == ZD1_STATUS_READY || *val == ZD1_STATUS_BL2)
            ready_seen = true;
    }
    return ok;
}
/* V read: lenLo lenHi (little-endian) then the data. Up to `max` bytes kept; *len = what the module sent. */
bool dm870_reg_read_v(uint8_t reg, uint8_t *buf, uint16_t max, uint16_t *len) {
    bool ok = false;
    int lo, hi;
    request(ZD1_WIRE_READ_V, reg);
    uint32_t end = tick_ms() + READ_MS;
    if (mod_skip_to(ZD1_WIRE_READ_V, end) && mod_skip_to(reg, end) && (lo = mod_get(end)) >= 0 &&
        (hi = mod_get(end)) >= 0) {
        uint16_t n = (uint16_t)(lo | (hi << 8)), i;
        ok = true;
        for (i = 0; i < n; i++) {
            int b = mod_get(end);
            if (b < 0) {
                ok = false;
                break;
            }
            if (i < max)
                buf[i] = (uint8_t)b;
        }
        *len = n;
    }
    mod_cs(false);
    return ok;
}

/* Status booting from our boot stub (reg 0x25 "zd1-boot"): it waits for which system slot to start (reg 0xC4, 0
 * until answered; modboot.c). Answered once per boot of the stub; true if answered now. */
static uint32_t bu_t, bu_deadline;
static bool stub_check(uint16_t st) {
    uint16_t n = 0, v = 0xFFFF;
    if (st != ZD1_STATUS_BOOTING)
        return false;
    if (!dm870_reg_read_v(ZD1_REG_VERSION_IMAGE, nbuf, sizeof nbuf, &n) || n < sizeof ZD1_BOOT_IDENT - 1 ||
        memcmp(nbuf, ZD1_BOOT_IDENT, sizeof ZD1_BOOT_IDENT - 1))
        return false;
    if (!dm870_reg_read(ZD1_REG_BOOT, &v) || v != 0)
        return false;
    uint16_t a = modboot_answer();
    dm870_reg_write(ZD1_REG_BOOT, (uint8_t)(a >> 8), (uint8_t)a);
    bu_deadline = tick_ms() + modboot_deadline_ms();
    return true;
}

/* The module is ready (or its link started again): find out whether it runs our OS (reg 0x25 VersionImage:
 * "zd1-os ..."; the stock app's says "I2014...") and whether it speaks this firmware's link protocol (reg 0xD5 =
 * ZD1_NATIVE_PROTO; another one: the base layer only, below, and a state the host sees). A boot the stub asked for
 * counts as good (modboot.c). Ours gets the hello: the MCU's version, the slot choice (reg 0xC4) and, once chosen,
 * the debug mode (then wifi_module_ready: the speaker's snapshot); nothing new is written to anything else. */
static void on_ready(void) {
    uint16_t n = 0, proto = 0;
    ours = dm870_reg_read_v(ZD1_REG_VERSION_IMAGE, nbuf, sizeof nbuf, &n) && n >= 7 && !memcmp(nbuf, "zd1-os ", 7);
    mod_proto = ours && dm870_reg_read(ZD1_REG_LINK_PROTO, &proto) ? (uint8_t)proto : 0;
    native = ours && mod_proto == ZD1_NATIVE_PROTO;
    if (last_status == ZD1_STATUS_READY)
        modboot_ready();
    if (!ours) {
        os_version[0] = 0;
        if (last_status == ZD1_STATUS_READY)
            log_puts("[mod] the stock software: no source\n");
        return;
    }
    copy_str(os_version, sizeof os_version, nbuf, n);
    log_puts("[mod] ");
    log_puts(os_version);
    log_puts(" link protocol ");
    log_dec(mod_proto);
    if (!native) {
        log_puts(" (this firmware: ");
        log_dec(ZD1_NATIVE_PROTO);
        log_puts("): no source until one is updated");
    }
    log_putc('\n');
    dm870_reg_write_v(ZD1_REG_MCU_VERSION, (const uint8_t *)g_fw_version, (uint16_t)strlen(g_fw_version));
    if (modboot_last())
        dm870_reg_write(ZD1_REG_BOOT, modboot_last(),
                        modboot_good()); /* what it runs, the good slot (a trial confirmed) */
    if (debug_mode != ZD1H_DEBUG_DEFAULT)
        dm870_reg_write(ZD1_REG_DEBUG, debug_mode, 0);
}

/* ---- notification drain (SPI_PROTOCOL 1.4): while PA20 is high, reg 0x0C (F) names the register with pending
 * data (b0); read it V-style. Each gets a one-line decode in the log, then wifi.c acts on it. ---- */
static void decode(uint8_t reg, const uint8_t *b, uint16_t n) {
    uint8_t b0 = n > 0 ? b[0] : 0, b1 = n > 1 ? b[1] : 0;
    switch (reg) {
    case ZD1_REG_STATUS:
        log_puts(b0 == ZD1_STATUS_READY >> 8     ? "ready"
                 : b0 == ZD1_STATUS_BOOTING >> 8 ? "booting"
                 : b0 == ZD1_STATUS_BL2 >> 8     ? "BL2"
                 : b0 == 0x25                    ? "AirPlay stopped"
                 : b0 == 0x27                    ? "module requests MCU update (stock: self-DFU on variant 2)"
                                                 : "status");
        break;
    case ZD1_REG_NET_STATUS: /* stock's rule: both chars != '0' = connected (live on wired Ethernet: "21") */
        log_puts(b0 != '0' && b1 != '0' ? "network CONNECTED" : "network not connected");
        break;
    case ZD1_REG_IP:
        log_puts("IP ");
        log_ascii(b, n);
        break;
    case ZD1_REG_NETMASK:
        log_puts("netmask ");
        log_ascii(b, n);
        break;
    case ZD1_REG_GATEWAY:
        log_puts("gateway ");
        log_ascii(b, n);
        break;
    case ZD1_REG_DEVICE_NAME:
        log_puts("device name ");
        log_ascii(b, n);
        break;
    case ZD1_REG_CONFIG:
        log_puts("configuration channel");
        break;
    case ZD1_REG_SHUTDOWN:
        log_puts("shutdown handshake");
        break;
    case ZD1_REG_PLAYER:
        log_puts("our receiver in front and its state");
        break;
    case ZD1_REG_VOLUME_SET:
        log_puts("our OS asks for a volume");
        break;
    case ZD1_REG_INTENT:
        log_puts("a receiver picked");
        break;
    default:
        log_puts(ours ? "(unknown)" : "(the stock software's: ignored)");
        break;
    }
}
/* what Info reports: the module's name and address as it notifies them */
static void track(uint8_t reg, const uint8_t *b, uint16_t n) {
    if (reg == ZD1_REG_IP)
        copy_str(mod_ip, sizeof mod_ip, b, n);
    else if (reg == ZD1_REG_DEVICE_NAME)
        copy_str(mod_name, sizeof mod_name, b, n);
    else if (reg == ZD1_REG_NET_STATUS && !(n >= 2 && zd1_net_connected(b[0], b[1])))
        mod_ip[0] = 0;
}
/* XModem to BL2 (the host link's XmFrame): its answers on 0x2C, as the drain finds them */
static uint8_t xm_rx[32];
static uint8_t xm_n;
static void xm_keep(const uint8_t *b, uint16_t n) {
    for (uint16_t i = 0; i < n; i++) {
        if (xm_n == sizeof xm_rx) {
            memmove(xm_rx, xm_rx + 1, sizeof xm_rx - 1);
            xm_n--;
        } /* keep the newest */
        xm_rx[xm_n++] = b[i];
    }
}

/* Up to 32 notifications, none started after DRAIN_MS (the rest stays pending: the next call) */
#define DRAIN_MS 100
static unsigned drain(void) {
    unsigned count = 0;
    uint32_t end = tick_ms() + DRAIN_MS;
    while (gpio_get_pin_value(PIN_MOD_REQ) && count < 32 && !tick_after(tick_ms(), end)) {
        uint16_t pend, n = 0;
        if (!dm870_reg_read(ZD1_REG_PENDING, &pend)) {
            log_puts("[notify] reg 0C read timeout\n");
            break;
        }
        uint8_t reg = (uint8_t)(pend >> 8);
        if (!dm870_reg_read_v(reg, nbuf, sizeof nbuf, &n)) {
            log_puts("[notify] reg ");
            log_hex8(reg);
            log_puts(" V read timeout\n");
            break;
        }
        if (reg == ZD1_REG_XM_MODULE) { /* BL2's XModem answers: for XmFrame */
            xm_keep(nbuf, n < sizeof nbuf ? n : sizeof nbuf);
            count++;
            continue;
        }
        uint16_t kept = n < sizeof nbuf ? n : sizeof nbuf;
        if (reg == ZD1_REG_STATUS && n >= 2 && (nbuf[0] << 8 | nbuf[1]) == ZD1_STATUS_BOOTING && bu == BU_READY) {
            /* the module restarted by itself: wait for its ready again (and answer its boot stub) */
            log_puts("[mod] restarted: booting\n");
            forget_module();
            last_status = ZD1_STATUS_BOOTING;
            bu = BU_WAIT;
            bu_t = tick_ms();
            bu_deadline = bu_t + 180000;
            stub_check(ZD1_STATUS_BOOTING);
            count++;
            continue;
        }
        if (reg == ZD1_REG_STATUS && n >= 2 && (nbuf[0] << 8 | nbuf[1]) == ZD1_STATUS_READY && bu == BU_READY) {
            /* ready again without a boot: the module's link started again (zd1-linkd restarted): greet it again,
             * so it knows who we are and what the speaker is */
            log_puts("[mod] its link started again: hello\n");
            on_ready();
            if (dm870_native())
                wifi_module_ready();
            count++;
            continue;
        }
        if (reg == ZD1_REG_STATE) { /* the module's State record: kept for the host */
            state_len = kept < sizeof state_rec ? kept : sizeof state_rec;
            memcpy(state_rec, nbuf, state_len);
            count++;
            continue;
        }
        if (reg == ZD1_REG_CONFIG && query == QUERY_WAIT) { /* the answer to the query: a record, a setting */
            if (query_rec) {
                rec_len = kept < sizeof rec_buf ? kept : sizeof rec_buf;
                memcpy(rec_buf, nbuf, rec_len);
            } else {
                copy_str(cfg_val, sizeof cfg_val, nbuf, kept);
                cfg_len = kept;
            }
            query_end(1);
            count++;
            continue;
        }
        log_puts("[notify] reg ");
        log_hex8(reg);
        log_puts(" len ");
        log_dec(n);
        log_puts(": ");
        for (uint16_t i = 0; i < n && i < 16; i++) {
            log_hex8(nbuf[i]);
            log_putc(' ');
        }
        if (n > 16)
            log_puts(".. ");
        log_puts(" -> ");
        decode(reg, nbuf, kept);
        log_putc('\n');
        track(reg, nbuf, kept);
        wifi_notify(reg, nbuf, kept);
        count++;
    }
    return count;
}

/* ---- non-blocking bring-up from the main loop (stock powers the module at every boot, FUN_8000dd58 timing):
 * PA04 high -> 1200 ms -> PA17 high + SPI -> 200 ms -> 1000 ms (T3) -> read 0x11 whenever PA20 is high until
 * 0x2100/0x2300 (stock gives up after 30 x 60 s). Our boot stub asks which slot (stub_check): a slot gets 90 s,
 * the rescue 180 s (modboot.c), then the slot counts as failed and the module is power-cycled (the next answer is
 * another one); anything else gets 180 s, then it is powered off. A power cycle leaves it off 500 ms first
 * (BU_OFF). ---- */
bool dm870_ready(void) { return bu == BU_READY || (mod_up && bu == BU_IDLE); }
static void bl2_poll(uint32_t now);
void dm870_start(void) {
    if (bu != BU_IDLE && bu != BU_READY)
        return;
    if (mod_up) {
        bu = BU_READY;
        return;
    }
    sysclk_enable_pba_module(SYSCLK_SPI0);
    gpio_set_pin_high(PIN_MOD_PWR);
    modcon_power(true);
    bu = BU_PWR;
    bu_t = tick_ms() + 1200;
    log_puts("[mod] powering up\n");
}
static void power_cycle(bool hold_only) {
    dm870_detach();
    hold = hold_only;
    bu = BU_OFF;
    bu_t = tick_ms() + 500;
}
static void bringup_poll(void) {
    uint32_t now = tick_ms();
    switch (bu) {
    case BU_OFF:
        if (!tick_after(now, bu_t))
            return;
        bu = BU_IDLE;
        dm870_start();
        return;
    case BU_PWR:
        if (!tick_after(now, bu_t))
            return;
        if (hold) { /* BL1's menu from the host, JTAG */
            hold = false;
            gpio_set_pin_high(PIN_MOD_RST);
            bu = BU_IDLE;
            log_puts("module powered, reset released; nothing else until `Module OFF`\n");
            if (op == OP_HOLD) {
                op = OP_NONE;
                op_result = 1;
            } else if (op == OP_ESC) {
                modcon_esc_start(); /* op_poll answers */
                esc_on = true;
            }
            return;
        }
        gpio_configure_pin(PIN_MOD_REQ, GPIO_DIR_INPUT);
        gpio_set_pin_high(PIN_MOD_RST);
        spi0_init();
        mod_up = true;
        bu = BU_RST;
        bu_t = now + 1200; /* 200 ms + stock's T3 1000 ms */
        bu_deadline = now + 180000;
        return;
    case BU_RST:
        if (!tick_after(now, bu_t))
            return;
        bu = op == OP_BL2 ? BU_BL2 : BU_WAIT;
        bu_t = now;
        return;
    case BU_BL2:
        bl2_poll(now);
        return;
    case BU_WAIT: {
        uint16_t st = 0;
        if (tick_after(now, bu_deadline)) {
            if (modboot_failed()) { /* a slot that doesn't come up: the next boot takes another */
                log_puts("[mod] NOT ready in time: power-cycling it\n");
                power_cycle(false);
                return;
            }
            log_puts("[mod] NOT ready in time, powering off\n");
            dm870_detach();
            bu = BU_IDLE;
            return;
        }
        if (!tick_after(now, bu_t) || !gpio_get_pin_value(PIN_MOD_REQ))
            return;
        if (!dm870_reg_read(ZD1_REG_STATUS, &st)) {
            bu_t = now + 300;
            return;
        }
        if (stub_check(st)) {
            bu_t = now + 20;
            return;
        }
        if (st == ZD1_STATUS_READY || st == ZD1_STATUS_BL2) {
            bu = BU_READY;
            log_puts("[mod] ready\n");
            on_ready();
            wifi_module_ready();
            return;
        }
        bu_t = now + 20;
        return;
    }
    default:
        return;
    }
}

static void op_poll(void);
static void query_poll(void);
void dm870_poll(void) {
    static uint32_t next; /* the drain's pace: at most every 50 ms */
    bringup_poll();
    op_poll();
    query_poll();
    if (bu != BU_IDLE && bu != BU_READY)
        return;
    if (!mod_up || !gpio_get_pin_value(PIN_MOD_REQ))
        return;
    if (!tick_after(tick_ms(), next))
        return;
    drain();
    next = tick_ms() + 50;
}

uint8_t dm870_state(void) {
    if (!dm870_powered())
        return ZD1H_MOD_STATE_OFF;
    if (!gpio_get_gpio_pin_output_value(PIN_MOD_RST))
        return ZD1H_MOD_STATE_RESET;
    if (bu != BU_IDLE && bu != BU_READY)
        return ZD1H_MOD_STATE_BOOTING;
    if (!ready_seen)
        return mod_up ? ZD1H_MOD_STATE_UP : ZD1H_MOD_STATE_BOOTING;
    if (last_status == ZD1_STATUS_BL2)
        return ZD1H_MOD_STATE_BL2;
    if (!ours)
        return ZD1H_MOD_STATE_STOCK;
    return native ? ZD1H_MOD_STATE_READY : ZD1H_MOD_STATE_MISMATCH;
}

/* ---- recovery: into BL2 ---- */
/* Recovery for custom module images (docs/module/boot-chain.md): power-cycle the module, then HCR Ctrl 0x28 = 21 00
 * ("reboot into BL2"). The stock app and zd1-linkd under Linux answer it with the BL2 mailbox + a watchdog reset; BL2
 * then reports status 0x2300. Every 200 ms: read 0x11; HCR is sent ONCE, at the first 0x2100 (ready; never 0x2000:
 * the stock app or our boot stub booting), then the SPI stays silent for 15 s; 0x2300 ends the wait. Like the stock
 * MCU, 0x11 is only read while the module raises the request line (PA20). One HCR + 15 s of silence reaches 0x2300
 * [HW]. Any other SPI traffic while BL2 starts up breaks it: an HCR every 200 ms leaves BL2 answering nothing [HW],
 * blind 0x11 polling keeps it from its menu [EMU 2026-10-19]; so no notifications are drained meanwhile (dm870_poll). An image that never gets ready: BL1's serial menu
 * (`Module` ESC, modcon.c). The power-up is the bring-up's (BU_PWR, BU_RST), then BU_BL2. */
#define HCR_TO_BL2 0x2100 /* reg 0x28: reboot into BL2 */
static uint32_t bl2_t0, bl2_quiet;
static uint16_t bl2_last;
static bool bl2_sent;
static void bl2_poll(uint32_t now) {
    uint16_t st = 0;
    if (!tick_after(now, bu_t))
        return;
    bu_t = now + 200;
    if (bl2_sent && !tick_after(now, bl2_quiet))
        return;
    if (!gpio_get_pin_value(PIN_MOD_REQ) || !dm870_reg_read(ZD1_REG_STATUS, &st))
        return;
    if (st != bl2_last) {
        log_puts("  t=");
        log_dec(now - bl2_t0);
        log_puts(" ms: reg 11 = ");
        log_hexn(st, 4);
        log_putc('\n');
        bl2_last = st;
    }
    stub_check(st);
    if (st == ZD1_STATUS_BL2) {
        log_puts("BL2 is running (status 0x2300)\n");
        bu = BU_IDLE;
        op = OP_NONE;
        op_result = 1;
        return;
    }
    if (st == ZD1_STATUS_READY && !bl2_sent) {
        dm870_reg_write(ZD1_REG_HCR, HCR_TO_BL2 >> 8, HCR_TO_BL2 & 0xFF);
        bl2_sent = true;
        bl2_quiet = now + 15000; /* no SPI while BL2 starts */
        log_puts("  HCR 0x21 sent; SPI quiet for 15 s\n");
    }
}

/* An op running: its end, or its time is up. Anything else that powers the module off ends it too. */
static void op_poll(void) {
    if (op == OP_NONE)
        return;
    bool ok = false;
    int esc = op == OP_ESC && esc_on ? modcon_esc_result() : -1;
    if (esc >= 0) {
        log_puts(esc ? "BL1 starts BL2: its web updater on 169.254.1.1 in ~11 s\n"
                     : "no BL1 menu, or BL2 not started: the module boots as usual\n");
        ok = esc;
    } else if (op == OP_ESC) {
        if (!tick_after(tick_ms(), op_deadline))
            return; /* the escape runs (or the power-cycle before it) */
        log_puts("the escape didn't end in time\n");
    } else if (op == OP_POWER && bu == BU_READY) {
        log_puts("module READY, reg11=");
        log_hexn(last_status, 4);
        log_putc('\n');
        ok = true;
    } else if (bu == BU_IDLE) /* powered off meanwhile, or (BL2) it gave up */
        log_puts(op == OP_POWER ? "NOT ready\n"
                 : op == OP_BL2 ? "no BL2 status (0x2300) seen\n"
                                : "powered off before the reset release\n");
    else if (tick_after(tick_ms(), op_deadline)) {
        if (op == OP_POWER) {
            log_puts("NOT ready (last reg11=");
            log_hexn(last_status, 4);
            log_puts("); powering off\n");
            dm870_detach();
        } else {
            log_puts("no BL2 status (0x2300) seen\n");
            bu = BU_IDLE;
        } /* left powered, as BL2 may come yet */
    } else
        return;
    op = OP_NONE;
    op_result = ok;
}

int dm870_op_result(void) { return op_result; }

void dm870_op(uint8_t o, uint32_t timeout_s) {
    op_result = 1;
    switch (o) {
    case ZD1H_MOD_OP_OFF:
        dm870_detach();
        log_puts("module powered OFF (detach: rst,20ms,pwr; SPI->in)\n");
        return;
    case ZD1H_MOD_OP_START:
        dm870_start();
        return;
    case ZD1H_MOD_OP_POWER:
        log_puts("full power-up...\n");
        if (bu == BU_READY) {
            log_puts("module READY, reg11=");
            log_hexn(last_status, 4);
            log_putc('\n');
            return;
        }
        if (bu == BU_IDLE && mod_up) { /* powered (HOLD, BL2): wait for its ready, as a bring-up */
            bu = BU_WAIT;
            bu_t = tick_ms();
            bu_deadline = bu_t + timeout_s * 1000u;
        } else if (bu == BU_IDLE)
            dm870_start();
        else
            log_puts("bring-up already running, ");
        log_puts("waiting for module ready (up to ");
        log_dec(timeout_s);
        log_puts(" s)...\n");
        op = OP_POWER;
        break;
    case ZD1H_MOD_OP_BL2:
        log_puts("power-cycling the module, HCR 0x21 once the app is ready, then waiting for BL2 (status 0x2300)...\n");
        power_cycle(false);
        bl2_t0 = tick_ms();
        bl2_last = 0xFFFF;
        bl2_sent = false;
        op = OP_BL2;
        break;
    case ZD1H_MOD_OP_RESTART:
        power_cycle(false);
        return;
    /* BL1's menu from the host, JTAG: no SPI, never powered off; answered at the reset release */
    case ZD1H_MOD_OP_HOLD:
        power_cycle(true);
        op = OP_HOLD;
        break;
    /* BL1's menu into BL2 (modcon.c), from the reset's release: whatever the image */
    case ZD1H_MOD_OP_ESC:
        log_puts("power-cycling the module, 0x02 into BL1's window from the reset's release...\n");
        power_cycle(true);
        op = OP_ESC;
        esc_on = false;
        break;
    default:
        op_result = 0;
        return;
    }
    op_deadline = tick_ms() + timeout_s * 1000u;
    op_result = -1;
}

/* ---- our OS: settings, records and the debug mode ---- */
/* The configuration channel (reg 0x45, codec::config): a query `Q zd1/KEY` (a setting; `U` writes "zd1/KEY" = value
 * first) or `Q zd1/@N` (a record, docs/module/settings.md), the module answering with a 0x45 notification. The
 * request writes the U; the Q goes from the next dm870_poll, which waits up to QUERY_MS for the answer while the main
 * loop goes on (hostif_poll asks dm870_query_result). One at a time. */
#define QUERY_MS 3000
static void query_poll(void) {
    if (query == QUERY_SEND) {
        if (!dm870_ours() || !dm870_reg_write_v(ZD1_REG_CONFIG, query_buf, query_len)) {
            query_end(0);
            return;
        }
        query = QUERY_WAIT;
        query_deadline = tick_ms() + QUERY_MS;
    } else if (query == QUERY_WAIT && tick_after(tick_ms(), query_deadline))
        query_end(0);
}
int dm870_query_result(void) { return query == QUERY_NONE ? query_result : -1; }

/* A message on the configuration channel: [op][length] "zd1/" name, into b; its length */
static uint16_t config_msg(uint8_t *b, uint8_t op, const char *name, unsigned n) {
    b[0] = op;
    b[1] = (uint8_t)(4 + n);
    memcpy(b + 2, "zd1/", 4);
    memcpy(b + 6, name, n);
    return (uint16_t)(6 + n);
}
/* the query for zd1/NAME, sent from dm870_poll; its answer goes to a record (rec) or the setting's value */
static void config_query(const char *name, unsigned n, bool rec) {
    query_len = config_msg(query_buf, 'Q', name, n);
    query_rec = rec;
    query = QUERY_SEND;
}

bool dm870_setting_begin(const char *key, const char *value) {
    static uint8_t b[sizeof query_buf + 128];
    unsigned kn = strlen(key), vn = value ? strlen(value) : 0;
    if (kn > CONFIG_NAME_MAX || vn > 128 || query != QUERY_NONE || !dm870_ours())
        return false;
    if (value) {
        uint16_t n = config_msg(b, 'U', key, kn);
        memcpy(b + n, value, vn);
        bool ok = dm870_reg_write_v(ZD1_REG_CONFIG, b, (uint16_t)(n + vn));
        memset(b, 0, sizeof b); /* a Wi-Fi key doesn't stay in RAM */
        if (!ok)
            return false;
    }
    config_query(key, kn, false);
    return true;
}
void dm870_setting_value(char *out, unsigned cap) { copy_str(out, cap, cfg_val, cfg_len); }

/* A record of the module OS (the host link's `Record`): State (0) from the module's last push (nothing asked); any
 * other fetched whole when the host starts reading it (offset 0) or asks for another, and served in pieces from the
 * copy. */
int dm870_record_begin(uint8_t rec, uint16_t offset) {
    if (rec == 0 || (offset != 0 && rec == rec_num))
        return 1;
    if (query != QUERY_NONE || !dm870_ours())
        return 0;
    char name[4] = {'@'};
    unsigned n = 1;
    if (rec >= 100)
        name[n++] = (char)('0' + rec / 100);
    if (rec >= 10)
        name[n++] = (char)('0' + rec / 10 % 10);
    name[n++] = (char)('0' + rec % 10);
    rec_len = 0;
    rec_num = rec;
    config_query(name, n, true);
    return -1;
}
void dm870_record_read(uint8_t rec, uint16_t offset, uint8_t *out, uint16_t cap, uint16_t *total, uint16_t *n) {
    const uint8_t *src = rec == 0 ? state_rec : rec_buf;
    uint16_t len = rec == 0 ? state_len : rec == rec_num ? rec_len : 0;
    *total = len;
    *n = offset >= len ? 0 : len - offset;
    if (*n > cap)
        *n = cap;
    memcpy(out, src + offset, *n);
}

void dm870_set_debug(uint8_t mode) {
    debug_mode = mode;
    if (dm870_ours())
        dm870_reg_write(ZD1_REG_DEBUG, mode, 0);
}

/* ---- XModem to BL2 over SPI (XmFrame): one V write to 0x2D, then BL2's answer on 0x2C ---- */
static uint32_t us_since(uint32_t cy) { return (Get_sys_count() - cy) / (sysclk_get_cpu_hz() / 1000000u); }
/* BL2's answer waited for here: up to XM_WAIT_MS, whatever the host asks (the watchdog's limit); a longer wait is
 * the host's, asking again with an empty XmFrame (dm870_poll's drain keeps the answer meanwhile) */
#define XM_WAIT_MS 500
void dm870_xm_send(const uint8_t *d, uint16_t n, uint16_t wait_ms, uint32_t *wire_us, uint32_t *wait_us,
                   uint32_t *read_us) {
    uint8_t had = xm_n;
    if (wait_ms > XM_WAIT_MS)
        wait_ms = XM_WAIT_MS;
    uint32_t t = Get_sys_count();
    dm870_reg_write_v(ZD1_REG_XM_HOST, d, n);
    *wire_us = us_since(t);
    *wait_us = *read_us = 0;
    t = Get_sys_count();
    while (us_since(t) < wait_ms * 1000u) {
        if (!gpio_get_pin_value(PIN_MOD_REQ))
            continue;
        *wait_us = us_since(t);
        uint32_t r = Get_sys_count();
        drain();
        *read_us = us_since(r);
        if (xm_n != had)
            return;
        *wait_us = 0; /* something else was pending: keep waiting */
    }
}
unsigned dm870_xm_take(uint8_t *out, unsigned cap) {
    unsigned n = xm_n < cap ? xm_n : cap;
    memcpy(out, xm_rx, n);
    xm_n = 0;
    return n;
}
