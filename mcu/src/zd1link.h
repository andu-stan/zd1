/* The MCU's side of the SPI protocol to the network module: what has to be code (the MCU runs C, the module Rust:
 * protocol/spi has the Rust), on top of the generated values (zd1link_gen.h: registers, kinds, DRC codes, the spec's
 * groups). mcu/test/zd1link_gen_test.c holds this to the Rust implementation, value for value.
 * Header-only, static inline: each helper folds into its caller, unused ones cost nothing. */
#ifndef ZD1LINK_H
#define ZD1LINK_H
#include "zd1link_gen.h"

/* ---- framing: every byte stuffed (0x27 -> 27 27, 0xFF -> 27 01), a raw 0xFF is idle (protocol/spi framing.rs) ---- */

/* one byte stuffed into out[0..1]; returns the count (1 or 2) */
static inline uint8_t zd1_stuff(uint8_t b, uint8_t out[2]) {
    if (b == ZD1_WIRE_ESC) {
        out[0] = ZD1_WIRE_ESC;
        out[1] = ZD1_WIRE_ESC;
        return 2;
    }
    if (b == ZD1_WIRE_IDLE) {
        out[0] = ZD1_WIRE_ESC;
        out[1] = 0x01;
        return 2;
    }
    out[0] = b;
    return 1;
}

typedef struct {
    bool esc;
} zd1_unstuff_t;
enum { ZD1_RX_IDLE, ZD1_RX_ESC, ZD1_RX_BYTE };

/* unstuffing, a raw byte at a time: ZD1_RX_BYTE (*b set), ZD1_RX_IDLE or ZD1_RX_ESC (wait for the next byte) */
static inline int zd1_unstuff(zd1_unstuff_t *u, uint8_t raw, uint8_t *b) {
    if (u->esc) {
        u->esc = false;
        *b = raw == 0x01 ? ZD1_WIRE_IDLE : raw;
        return ZD1_RX_BYTE;
    }
    if (raw == ZD1_WIRE_IDLE)
        return ZD1_RX_IDLE;
    if (raw == ZD1_WIRE_ESC) {
        u->esc = true;
        return ZD1_RX_ESC;
    }
    *b = raw;
    return ZD1_RX_BYTE;
}

/* ---- payloads (protocol/spi codec.rs) ---- */

/* the MCU's volume step (0..71) for an attenuation (0x40 / 0xE4 vv: 0 loudest .. 240, more = mute), and back */
static inline uint8_t zd1_volume_step_from_att(uint8_t att) {
    if (att > 0xF0)
        return 0;
    unsigned d = ((unsigned)att + 3) * 70 / 240;
    return d >= ZD1_VOLUME_STEPS ? 0 : (uint8_t)(ZD1_VOLUME_STEPS - d);
}
static inline uint8_t zd1_volume_att_from_step(uint8_t step) {
    unsigned d = ZD1_VOLUME_STEPS - (step > ZD1_VOLUME_STEPS ? ZD1_VOLUME_STEPS : step);
    return d >= ZD1_VOLUME_STEPS ? ZD1_VOLUME_MUTE : (uint8_t)(d * 240 / 70);
}

/* network status (0x32): both characters != '0' = connected */
static inline bool zd1_net_connected(uint8_t b0, uint8_t b1) { return b0 != '0' && b1 != '0'; }

/* the play state (0x47 DRC XML): finds ZD1_PLAY_PATTERN and the icon after it in XML that arrives in pieces */
enum { ZD1_PLAY_NONE, ZD1_PLAY_EMPTY, ZD1_PLAY_PLAYING, ZD1_PLAY_PAUSED };
typedef struct {
    uint8_t matched, vn;
    bool in_val;
    char val[3];
} zd1_play_matcher_t;

/* the value (two letters or more) is that icon ("pl"[0] folds to 'p': no strings in flash) */
#define ZD1_PLAY_IS(m, icon) ((m)->val[0] == icon[0] && (m)->val[1] == icon[1])

/* ZD1_PLAY_NONE until a whole icon value is by */
static inline int zd1_play_push(zd1_play_matcher_t *m, char c) {
    static const char pat[] = ZD1_PLAY_PATTERN;
    if (m->in_val) {
        if (c == '<' || m->vn == sizeof m->val) {
            m->in_val = false;
            m->matched = c == '<' ? 1 : 0;
            if (m->vn < 2)
                return ZD1_PLAY_NONE;
            if (ZD1_PLAY_IS(m, ZD1_PLAY_ICON_PLAYING))
                return ZD1_PLAY_PLAYING;
            if (ZD1_PLAY_IS(m, ZD1_PLAY_ICON_PAUSED))
                return ZD1_PLAY_PAUSED;
            if (ZD1_PLAY_IS(m, ZD1_PLAY_ICON_EMPTY))
                return ZD1_PLAY_EMPTY;
            return ZD1_PLAY_NONE;
        }
        m->val[m->vn++] = c;
        return ZD1_PLAY_NONE;
    }
    if (c == pat[m->matched]) {
        if (++m->matched == sizeof pat - 1) {
            m->in_val = true;
            m->vn = 0;
            m->matched = 0;
        }
    } else
        m->matched = c == pat[0] ? 1 : 0;
    return ZD1_PLAY_NONE;
}

#endif
