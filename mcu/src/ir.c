#include <sysclk.h>
#include <gpio.h>
#include <cycle_counter.h>
#include "zd1_board.h"
#include "log.h"
#include "inputs.h"
#include "tick.h"
#include "ir.h"

#define T_US      444
#define GAP_US    3000 /* idle longer than this ends a frame */
#define REPEAT_MS 200  /* same toggle bit within this = key held */

/* ISR side: ring of (level that just ended, duration in us) */
static volatile uint16_t dur[256];
static volatile uint8_t lvl[256];
static volatile uint8_t wh, rh;
static volatile uint32_t last_edge_cy;
static bool frame_open;
static uint8_t halves[64];
static int nh; /* half-bit levels of the current frame (1 = idle/space, 0 = mark) */

static struct {
    uint8_t addr, cmd, toggle;
    uint32_t at;
} last_frame;
static uint16_t n_frames; /* for Diag */
static int last_toggle = -1;
static uint32_t last_at;

ISR(ir_isr, AVR32_GPIO_IRQ_GROUP, 1) {
    uint32_t now = Get_sys_count();
    uint32_t us = (now - last_edge_cy) / (sysclk_get_cpu_hz() / 1000000u);
    bool level_now = gpio_get_pin_value(PIN_IR);
    last_edge_cy = now;
    uint8_t n = wh + 1;
    if (n != rh) {
        dur[wh] = us > 0xFFFF ? 0xFFFF : (uint16_t)us;
        lvl[wh] = !level_now;
        wh = n;
    }
    gpio_clear_pin_interrupt_flag(PIN_IR);
}

void ir_init(void) {
    gpio_configure_pin(PIN_IR, GPIO_DIR_INPUT);
    last_edge_cy = Get_sys_count();
    irq_register_handler(ir_isr, AVR32_GPIO_IRQ_0 + PIN_IR / 8, 1);
    gpio_enable_pin_interrupt(PIN_IR, GPIO_PIN_CHANGE);
}

static void deliver(uint8_t addr, uint8_t cmd, uint8_t toggle) {
    uint32_t now = tick_ms();
    bool repeat = (int)toggle == last_toggle && now - last_at < REPEAT_MS;
    last_toggle = toggle;
    last_at = now;
    last_frame.addr = addr;
    last_frame.cmd = cmd;
    last_frame.toggle = toggle;
    last_frame.at = now;
    n_frames++;
    if (addr != IR_ADDR)
        return;
    key_event_t k = KEY_NONE;
    switch (cmd) {
    case 0x0C:
        k = repeat ? KEY_NONE : KEY_STANDBY;
        break; /* standby: once per press */
    case 0x10:
        k = KEY_VOLUP;
        break; /* volume repeats while held */
    case 0x11:
        k = KEY_VOLDN;
        break;
    case 0x20:
        k = repeat ? KEY_NONE : KEY_NEXT;
        break;
    case 0x21:
        k = repeat ? KEY_NONE : KEY_PREV;
        break;
    case 0x2C:
        k = repeat ? KEY_NONE : KEY_PLAYPAUSE;
        break;
    }
    if (k)
        inputs_inject_key(k);
}

/* frame = half-bits: leader 6 mark + 2 space, start '1', 3 mode bits, trailer (2+2), 8 addr + 8 cmd bits */
static void decode(void) {
    int i = 0, b;
    uint32_t v = 0;
    if (nh < 8 + 2 + 6 + 4 + 32)
        return;
    for (int k = 0; k < 6; k++)
        if (halves[i++] != 0)
            return; /* 6T mark */
    for (int k = 0; k < 2; k++)
        if (halves[i++] != 1)
            return; /* 2T space */
#define BIT(out)                                                                                                       \
    do {                                                                                                               \
        uint8_t a = halves[i], c = halves[i + 1];                                                                      \
        i += 2;                                                                                                        \
        if (a == c)                                                                                                    \
            return;                                                                                                    \
        out = (a == 0);                                                                                                \
    } while (0)
    BIT(b);
    if (!b)
        return; /* start bit = 1 */
    for (int k = 0; k < 3; k++) {
        BIT(b);
        if (b)
            return;
    } /* mode 0 */
    uint8_t ta = halves[i], tb = halves[i + 2];
    if (halves[i + 1] != ta || halves[i + 3] != tb || ta == tb)
        return;
    i += 4;
    uint8_t toggle = (ta == 0);
    for (int k = 0; k < 16; k++) {
        BIT(b);
        v = (v << 1) | b;
    }
#undef BIT
    deliver((uint8_t)(v >> 8), (uint8_t)v, toggle);
}

static void add_halves(uint8_t level, uint16_t us) {
    unsigned n = (us + T_US / 2) / T_US; /* round to whole T */
    if (n == 0)
        n = 1;
    while (n-- && nh < (int)sizeof halves)
        halves[nh++] = level;
}

void ir_poll(uint32_t now) {
    (void)now;
    while (rh != wh) {
        uint8_t l = lvl[rh];
        uint16_t us = dur[rh];
        rh++;
        if (l == 1 && us > GAP_US) { /* long idle: closes a frame, next mark opens one */
            if (frame_open)
                decode();
            frame_open = true;
            nh = 0;
            continue;
        }
        if (frame_open)
            add_halves(l, us);
    }
    /* a frame is complete once the line has been idle for GAP_US */
    if (frame_open && nh && gpio_get_pin_value(PIN_IR)) {
        /* last edge first, then the clock: an edge interrupt between the two reads would otherwise make the
         * difference negative (= a huge idle time) and end the frame early (found by the Renode emulator) */
        uint32_t last = last_edge_cy;
        uint32_t idle_us = (Get_sys_count() - last) / (sysclk_get_cpu_hz() / 1000000u);
        if (idle_us > GAP_US) {
            add_halves(1, T_US);
            decode();
            nh = 0;
            frame_open = true;
        }
    }
}

bool ir_last(uint8_t *addr, uint8_t *cmd, bool *toggle, uint32_t *at_ms, uint16_t *count) {
    *addr = last_frame.addr;
    *cmd = last_frame.cmd;
    *toggle = last_frame.toggle;
    *at_ms = last_frame.at;
    *count = n_frames;
    return n_frames != 0;
}
