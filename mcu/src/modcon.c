/* The network module's console on USART1 (modcon.h): into our log, line by line, or to the host link while a host has
 * it open there. Both directions go through a ring the USART's interrupt serves: RX from RXRDY
 * (115200 is 87 us a byte and the main loop can take longer than that), TX from TXRDY, so the line runs back to back
 * (11.5 KB/s) however seldom the main loop refills it; fed from the main loop alone it moves one byte per pass,
 * ~1 KB/s [HW 2026-10-26]. */
#include <sysclk.h>
#include <gpio.h>
#include <intc.h>
#include "log.h"
#include "tick.h"
#include "modcon.h"

#define RING 2048u
static volatile uint8_t ring[RING];
static volatile uint32_t rh, rt; /* ISR writes rt, readers advance rh */
#define TXRING 1024u
static volatile uint8_t txring[TXRING];
static volatile uint32_t th, tt; /* the main loop writes tt, the ISR advances th */
static volatile uint32_t overruns;
static bool up;
static bool taken; /* the updater has the line (update.c) */
/* the host link's side: the module's bytes for POLL (the main loop writes ht, the USB interrupt advances hh) */
#define HRING 2048u
static volatile uint8_t hring[HRING];
static volatile uint32_t hh, ht, host_lost;
static volatile bool host_open;

ISR(modcon_isr, AVR32_USART1_IRQ / 32, 1) {
    volatile avr32_usart_t *u = &AVR32_USART1;
    uint32_t csr = u->csr;
    if (csr & (AVR32_USART_CSR_OVRE_MASK | AVR32_USART_CSR_FRAME_MASK)) {
        overruns++;
        u->cr = AVR32_USART_CR_RSTSTA_MASK;
    }
    if (csr & AVR32_USART_CSR_RXRDY_MASK) {
        uint8_t c = (uint8_t)u->rhr;
        if (rt - rh < RING) {
            ring[rt % RING] = c;
            rt++;
        }
    }
    if ((u->imr & AVR32_USART_IMR_TXRDY_MASK) && (csr & AVR32_USART_CSR_TXRDY_MASK)) {
        if (th != tt) {
            u->thr = txring[th % TXRING];
            th++;
        } else
            u->idr = AVR32_USART_IDR_TXRDY_MASK; /* empty: the main loop enables it again */
    }
}

static void modcon_init(void) {
    if (up)
        return;
    volatile avr32_usart_t *u = &AVR32_USART1;
    sysclk_enable_pba_module(SYSCLK_USART1);
    u->idr = 0xFFFFFFFF;
    u->cr = AVR32_USART_CR_RSTRX_MASK | AVR32_USART_CR_RSTTX_MASK | AVR32_USART_CR_RSTSTA_MASK;
    u->mr = (3 << AVR32_USART_MR_CHRL_OFFSET) | (4 << AVR32_USART_MR_PAR_OFFSET); /* 8 bits, no parity, 1 stop */
    u->brgr = (sysclk_get_pba_hz() + 8 * 115200) / (16 * 115200);                 /* 48 MHz: 26, +0.16 % */
    static const gpio_map_t pins = {
        {AVR32_USART1_RXD_0_0_PIN, AVR32_USART1_RXD_0_0_FUNCTION},
        {AVR32_USART1_TXD_0_0_PIN, AVR32_USART1_TXD_0_0_FUNCTION},
    };
    u->cr = AVR32_USART_CR_TXEN_MASK; /* the line idles high before the pin is handed over: no false start bit */
    gpio_enable_module(pins, 2);
    irq_register_handler(modcon_isr, AVR32_USART1_IRQ, 1);
    rh = rt = th = tt = 0;
    u->cr = AVR32_USART_CR_RXEN_MASK;
    u->ier = AVR32_USART_IER_RXRDY_MASK;
    up = true;
}

static int modcon_getc(void) {
    if (rh == rt)
        return -1;
    uint8_t c = ring[rh % RING];
    rh++;
    return c;
}

/* Into the send ring from the main loop (the host link's ConsoleWrite, BL1's escape, the updater; the USART's
 * interrupt only takes): as many as fit. */
static unsigned tx_put(const uint8_t *d, unsigned n) {
    unsigned room = TXRING - (tt - th);
    if (n > room)
        n = room;
    for (unsigned i = 0; i < n; i++) {
        txring[tt % TXRING] = d[i];
        tt++;
    }
    AVR32_USART1.ier = AVR32_USART_IER_TXRDY_MASK;
    return n;
}

static void line_out(void);
static unsigned ln;
/* opened afresh: what came before (in the log) dropped, the count of lost bytes from zero; closed: the count stays */
void modcon_host_open(bool open) {
    if (open && ln)
        line_out(); /* the log's last line before the host's bytes */
    irqflags_t f = cpu_irq_save();
    if (open) {
        hh = ht;
        host_lost = 0;
    }
    host_open = open;
    cpu_irq_restore(f);
}
bool modcon_host(void) { return host_open; }
uint32_t modcon_host_lost(void) { return host_lost; }
unsigned modcon_host_avail(void) { return ht - hh; }
unsigned modcon_host_take(uint8_t *out, unsigned n) {
    uint32_t avail = ht - hh, k = avail < n ? avail : n;
    for (uint32_t i = 0; i < k; i++)
        out[i] = hring[(hh + i) % HRING];
    hh += k;
    return k;
}
unsigned modcon_host_put(const uint8_t *d, unsigned n) { return host_open && up && !taken ? tx_put(d, n) : 0; }

/* BL1's escape (modcon.h): the menu's line and BL1's start of BL2 matched in the byte stream, 3 s for each */
enum { ESC_IDLE, ESC_SEND, ESC_MENU, ESC_SENT, ESC_DONE, ESC_FAILED };
static uint8_t esc, esc_menu_i, esc_start_i;
static uint32_t esc_t, esc_until;
static const char ESC_MENU_LINE[] = "4) Start Bootloader Level 2", ESC_STARTING[] = "Starting Bootloader Level 2";

static bool match(const char *pat, uint8_t *i, uint8_t c) {
    if (c != (uint8_t)pat[*i]) {
        *i = c == (uint8_t)pat[0];
        return false;
    }
    if (pat[++*i])
        return false;
    *i = 0;
    return true;
}
void modcon_esc_start(void) {
    esc = ESC_SEND;
    esc_menu_i = esc_start_i = 0;
    esc_t = tick_ms();
    esc_until = esc_t + 3000;
}
int modcon_esc_result(void) { return esc == ESC_DONE ? 1 : esc == ESC_FAILED ? 0 : -1; }
static void esc_byte(uint8_t c) {
    if (esc == ESC_SEND && match(ESC_MENU_LINE, &esc_menu_i, c)) {
        esc = ESC_MENU; /* the menu gets 300 ms to finish */
        esc_t = tick_ms() + 300;
    }
    if (esc >= ESC_SEND && esc <= ESC_SENT && match(ESC_STARTING, &esc_start_i, c))
        esc = ESC_DONE;
}
static void esc_poll(void) {
    uint32_t now = tick_ms();
    if (esc == ESC_SEND && tick_after(now, esc_until))
        esc = ESC_FAILED;
    else if (esc == ESC_SEND && !tick_after(esc_t, now)) {
        static const uint8_t stx = 0x02;
        tx_put(&stx, 1);
        esc_t = now + 20;
    } else if (esc == ESC_MENU && tick_after(now, esc_t)) {
        tx_put((const uint8_t *)"4\r", 2);
        esc = ESC_SENT;
        esc_until = now + 3000;
    } else if (esc == ESC_SENT && tick_after(now, esc_until))
        esc = ESC_FAILED;
}

/* The module's lines into our log: printable text, a line at a time (one that doesn't end, a prompt, after 300 ms) */
static char line[161];
static uint32_t line_t;
static void line_out(void) {
    line[ln] = 0;
    log_puts(line);
    log_putc('\n');
    ln = 0;
}
static void line_add(uint8_t c) {
    if (c == '\n') {
        line_out();
        return;
    }
    if ((c >= 0x20 && c < 0x7F) || c == '\t')
        line[ln++] = (char)c;
    if (ln == sizeof line - 1)
        line_out();
    line_t = tick_ms();
}

/* The module's bytes, never blocking: to the host link while a host has the console open, else to the log; BL1's
 * escape watches them. (The host's bytes come by ConsoleWrite: modcon_host_put.) */
void modcon_poll(void) {
    if (!up || taken)
        return;
    for (int i = 0; i < 256 && rh != rt; i++) {
        uint8_t c = (uint8_t)modcon_getc();
        if (esc)
            esc_byte(c);
        if (!host_open)
            line_add(c);
        else if (ht - hh < HRING) { /* the host has them raw: not in the log twice */
            hring[ht % HRING] = c;
            ht++;
        } else
            host_lost++;
    }
    if (ln && tick_after(tick_ms(), line_t + 300))
        line_out();
    esc_poll();
}

/* The module powered: the UART up (its RX into our log). Off: the pins back to inputs, nothing drives the module's
 * RXD while it has no supply. */
void modcon_power(bool on) {
    if (on) {
        modcon_init();
        rh = rt;
        ln = 0;
        return;
    }
    if (!up || taken)
        return;
    volatile avr32_usart_t *u = &AVR32_USART1;
    u->idr = 0xFFFFFFFF;
    u->cr = AVR32_USART_CR_RXDIS_MASK | AVR32_USART_CR_TXDIS_MASK;
    gpio_configure_pin(AVR32_USART1_RXD_0_0_PIN, GPIO_DIR_INPUT);
    gpio_configure_pin(AVR32_USART1_TXD_0_0_PIN, GPIO_DIR_INPUT);
    if (ln)
        line_out();
    up = false;
}

uint32_t modcon_overruns(void) { return overruns; }

/* The divisor in eighths (CD + FP/8, 16x oversampling): 48 MHz makes 1 and 2 Mbaud exact, 115200, 230400 and
 * 460800 within 0.16 % (460800 is 7 % off without the fraction, which the module can't read [HW 2026-10-26]) */
static uint32_t eighths(uint32_t b) { return (sysclk_get_pba_hz() + b) / (2 * b); }
uint32_t modcon_baud(uint32_t b) {
    uint32_t x = eighths(b);
    return x >= 8 ? sysclk_get_pba_hz() / (2 * x) : 0;
}
static void baud(uint32_t b) {
    uint32_t x = eighths(b);
    AVR32_USART1.brgr = (x >> 3) << AVR32_USART_BRGR_CD_OFFSET | (x & 7) << AVR32_USART_BRGR_FP_OFFSET;
}
void modcon_take(uint32_t b) {
    modcon_init();
    AVR32_USART1.idr = AVR32_USART_IDR_TXRDY_MASK;
    taken = true;
    th = tt;
    rh = rt;
    baud(b);
}
void modcon_give(void) {
    AVR32_USART1.idr = AVR32_USART_IDR_TXRDY_MASK;
    th = tt; /* the update is over: anything not sent yet is moot */
    for (uint32_t n = 0; n < 100000 && !(AVR32_USART1.csr & AVR32_USART_CSR_TXEMPTY_MASK); n++) {
    }
    baud(115200);
    rh = rt;
    taken = false;
}
int modcon_rx(void) { return modcon_getc(); }
unsigned modcon_tx_room(void) { return TXRING - (tt - th); }
void modcon_tx(const uint8_t *d, unsigned n) { tx_put(d, n); }
