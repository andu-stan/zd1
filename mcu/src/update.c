/* Updating the network module's system (update.h; docs/module/updater.md, the protocol on the line: zd1-update's
 * source). BEGIN asks the module over SPI (reg 0xC5: size, baud, sha256, the stamp; zd1-linkd starts zd1-update),
 * takes the console UART at that baud (modcon.c) and points the bulk pipe at us. On the line the module talks in
 * lines, 0x02 TEXT \n (ready SLOT, ok SLOT ID, error WHY). J2 has no flow control, so the module paces us: frames
 * of up to 512 bytes, 7E SEQ LEN(2, LE) DATA CRC32(4, LE; over SEQ, LEN and DATA), FRAMES of them in flight. The
 * module answers each good one with 0E SEQ, a bad or missing one with 15 SEQ (that frame is the next it wants): we
 * send again from there (go-back-N; the frames stay here until answered): at 1 Mbaud the module loses bytes while
 * it writes the NAND [HW 2026-10-26]. The host's data waits in the pipe (USB NAKs) until then. */
#include <string.h>
#include "zd1link.h"
#include "dm870.h"
#include "modcon.h"
#include "pipe.h"
#include "tick.h"
#include "log.h"
#include "update.h"
#include "util.h"

#define FRAME     512u
#define FRAMES    4u /* in flight: 2 KB; the module's tty buffer is 4 KB */
#define SOF       0x7E
#define FACK      0x0E
#define FNAK      0x15
#define READY_MS  15000u /* zd1-linkd starts zd1-update, it takes the line */
#define QUIET_MS  15000u /* streaming: no answer for this long (the module gives up after 10 s without data) */
#define FINISH_MS 60000u /* all sent: the rest of the unpack, the hash, the stamp */

static uint8_t state = ZD1H_UPDATE_STATE_IDLE, slot;
static uint32_t size, sent, on_wire, deadline, began;
static char text[97];
static char line[97];
static uint8_t ln;
static bool in_line;

/* frame i lives in frm[i % FRAMES] from when it is built (nfr counts them) until answered (acked) */
static uint8_t frm[FRAMES][FRAME];
static uint16_t flen[FRAMES];
static uint32_t fcrc[FRAMES];
static uint32_t nfr, acked, txi, txo, txmax, fill, taken, resends;
static uint8_t tok; /* FACK or FNAK: its SEQ comes next */

static void set_text(const char *s) { copy_str(text, sizeof text, s, STR_ALL); }

bool update_busy(void) { return state >= ZD1H_UPDATE_STATE_STARTING && state <= ZD1H_UPDATE_STATE_FINISHING; }

static void finish(uint8_t st, const char *why) {
    state = st;
    set_text(why);
    modcon_give();
    pipe_to(ZD1H_SINK_DISCARD);
    log_puts(st == ZD1H_UPDATE_STATE_DONE ? "[update] done: " : "[update] FAILED: ");
    log_puts(why);
    log_puts(" (");
    log_dec(sent);
    log_puts(" bytes, ");
    log_dec((tick_ms() - began) / 1000);
    log_puts(" s, ");
    log_dec(resends);
    log_puts(" resends)\n");
}

const char *update_begin(const zd1h_update_t *q) {
    static uint8_t b[4 + 4 + 32 + 64];
    if (update_busy())
        return "an update is running (ABORT it first)";
    if (!dm870_ours())
        return "the module doesn't run our OS";
    if (q->sha256_len != 32 || !q->size)
        return "size and a 32-byte sha256";
    uint32_t baud = q->baud ? q->baud : 1000000u, real = modcon_baud(baud);
    if (!real || (real > baud ? real - baud : baud - real) > baud / 50)
        return "a baud the MCU can't make within 2 %";
    unsigned idn = strlen(q->id);
    for (int i = 0; i < 4; i++) {
        b[i] = (uint8_t)(q->size >> (8 * i));
        b[4 + i] = (uint8_t)(baud >> (8 * i));
    }
    memcpy(b + 8, q->sha256, 32);
    memcpy(b + 40, q->id, idn);
    if (!dm870_reg_write_v(ZD1_REG_UPDATE, b, (uint16_t)(40 + idn)))
        return "the module didn't acknowledge the request (three tries)";
    pipe_to(ZD1H_SINK_MODULE);
    modcon_take(baud);
    size = q->size;
    sent = on_wire = 0;
    slot = ZD1H_SLOT_NONE;
    ln = 0;
    in_line = false;
    nfr = acked = txi = txo = txmax = fill = taken = resends = 0;
    tok = 0;
    state = ZD1H_UPDATE_STATE_STARTING;
    began = tick_ms();
    deadline = began + READY_MS;
    set_text("asked the module's updater to start");
    log_puts("[update] ");
    log_dec(size);
    log_puts(" bytes at ");
    log_dec(baud);
    log_puts(" baud\n");
    return 0;
}

void update_abort(const char *why) {
    if (update_busy())
        finish(ZD1H_UPDATE_STATE_FAILED, why);
}

void update_state(zd1h_update_reply_t *r) {
    r->state = state;
    r->sent = sent;
    r->on_wire = on_wire;
    r->size = size;
    r->slot = slot;
    memcpy(r->text, text, sizeof r->text);
}

static uint8_t slot_of(char c) { return c == 'a' ? ZD1H_SLOT_A : c == 'b' ? ZD1H_SLOT_B : ZD1H_SLOT_NONE; }

static void on_line(uint32_t now) {
    static const char READY[] = "zd1-update ready ", OK[] = "zd1-update ok ", ERR[] = "zd1-update error ";
    if (!strncmp(line, READY, sizeof READY - 1) && state == ZD1H_UPDATE_STATE_STARTING) {
        slot = slot_of(line[sizeof READY - 1]);
        state = ZD1H_UPDATE_STATE_STREAMING;
        deadline = now + QUIET_MS;
        set_text(line);
        log_puts("[update] the module's updater is ready: into slot ");
        log_putc(line[sizeof READY - 1]);
        log_putc('\n');
    } else if (!strncmp(line, OK, sizeof OK - 1)) {
        slot = slot_of(line[sizeof OK - 1]);
        finish(ZD1H_UPDATE_STATE_DONE, line);
    } else if (!strncmp(line, ERR, sizeof ERR - 1)) {
        finish(ZD1H_UPDATE_STATE_FAILED, line);
    }
}

/* the module's answer about frame SEQ (the low byte of a frame's number) */
static void on_answer(uint8_t t, uint8_t seq, uint32_t now) {
    uint32_t d = (uint8_t)(seq - (uint8_t)acked);
    if (t == FACK && d < txmax - acked) { /* that one and those before it arrived */
        acked += d + 1;
        sent = acked * FRAME < size ? acked * FRAME : size;
        deadline = now + QUIET_MS;
    } else if (t == FNAK && d <= nfr - acked) { /* the ones before it arrived; from it on again */
        acked += d;
        sent = acked * FRAME < size ? acked * FRAME : size;
        if (txi > acked || txo) {
            resends++;
            txi = acked;
            txo = 0;
        }
        deadline = now + QUIET_MS;
    }
}

/* frames from the pipe, as many as may be in flight */
static void build(void) {
    const uint8_t *d;
    uint16_t n;
    while (nfr < acked + FRAMES && taken < size && (n = pipe_peek(&d)) > 0) {
        unsigned s = nfr % FRAMES;
        uint32_t k = n;
        if (k > FRAME - fill)
            k = FRAME - fill;
        if (k > size - taken)
            k = size - taken;
        memcpy(frm[s] + fill, d, k);
        pipe_take((uint16_t)k);
        fill += k;
        taken += k;
        if (fill == FRAME || taken == size) {
            uint8_t h[3] = {(uint8_t)nfr, (uint8_t)fill, (uint8_t)(fill >> 8)};
            flen[s] = (uint16_t)fill;
            fcrc[s] = ~crc32_add(crc32_add(0xFFFFFFFFu, h, 3), frm[s], fill);
            nfr++;
            fill = 0;
        }
    }
}

/* the built frames onto the line, as much as the UART's ring takes */
static void transmit(void) {
    unsigned room;
    while (txi < nfr && (room = modcon_tx_room()) > 0) {
        unsigned s = txi % FRAMES, len = flen[s], wire = 4 + len + 4, n;
        uint8_t e[4];
        const uint8_t *p;
        if (txo < 4) {
            e[0] = SOF;
            e[1] = (uint8_t)txi;
            e[2] = (uint8_t)len;
            e[3] = (uint8_t)(len >> 8);
            p = e + txo;
            n = 4 - txo;
        } else if (txo < 4 + len) {
            p = frm[s] + (txo - 4);
            n = 4 + len - txo;
        } else {
            for (int i = 0; i < 4; i++)
                e[i] = (uint8_t)(fcrc[s] >> (8 * i));
            p = e + (txo - 4 - len);
            n = wire - txo;
        }
        if (n > room)
            n = room;
#ifdef ZD1_EMU
        /* the emulator's line loses nothing: spoil some first sendings, as the unit's did, to exercise the resends */
        if (txi == txmax && txo == 4 + len && txi % 32 == 7) {
            e[0] ^= 1;
        } /* a bad CRC */
        if (txi == txmax && txo == 4 && txi % 32 == 23 && n > 100) {
            modcon_tx(p, 100);
            txo += 101;
            continue;
        } /* a lost byte */
#endif
        modcon_tx(p, n);
        txo += n;
        on_wire += n;
        if (txo == wire) {
            txi++;
            txo = 0;
            if (txi > txmax)
                txmax = txi;
        }
    }
}

void update_poll(uint32_t now) {
    if (!update_busy())
        return;
    int c;
    while (update_busy() && (c = modcon_rx()) >= 0) {
        if (tok) {
            on_answer(tok, (uint8_t)c, now);
            tok = 0;
        } else if (in_line) {
            if (c == '\n') {
                line[ln] = 0;
                in_line = false;
                on_line(now);
            } else if (ln < sizeof line - 1)
                line[ln++] = (char)c;
        } else if (c == 0x02) {
            in_line = true;
            ln = 0;
        } else if (state >= ZD1H_UPDATE_STATE_STREAMING && (c == FACK || c == FNAK))
            tok = (uint8_t)c;
        /* anything else: the kernel's console, noise from the baud switch */
    }
    if (state == ZD1H_UPDATE_STATE_STREAMING) {
        build();
        transmit();
        if (acked * FRAME >= size) {
            state = ZD1H_UPDATE_STATE_FINISHING;
            deadline = now + FINISH_MS;
            set_text("all sent: the module checks and stamps it");
        }
    }
    if (update_busy() && tick_after(now, deadline))
        finish(ZD1H_UPDATE_STATE_FAILED, state == ZD1H_UPDATE_STATE_STARTING ? "the module's updater didn't say ready"
                                         : state == ZD1H_UPDATE_STATE_STREAMING ? "the module stopped answering"
                                                                                : "the module didn't say how it ended");
}
