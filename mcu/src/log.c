/* The log (log.h): a ring the main loop writes and the host link's interrupt reads (POLL). */
#include "tick.h"
#include "log.h"

#define LOG_SIZE 4096u /* a power of two: the indexes run free */
static volatile uint8_t logbuf[LOG_SIZE];
static volatile uint32_t log_head, log_tail;
static bool reader; /* a host reads it */

void log_reader(bool present) { reader = present; }
uint32_t log_written(void) { return log_head; }
uint32_t log_taken(void) { return log_tail; }

unsigned log_take(uint8_t *out, unsigned cap) {
    uint32_t avail = log_head - log_tail, k = avail < cap ? avail : cap;
    for (uint32_t i = 0; i < k; i++)
        out[i] = logbuf[(log_tail + i) % LOG_SIZE];
    log_tail += k;
    return k;
}

/* A host that reads gets up to 50 ms to make room when the log is full (a long command's output arrives whole);
 * one that doesn't counts as gone until it polls again. With no host the oldest text goes. */
void log_putc(char c) {
    if (log_head - log_tail >= LOG_SIZE && reader) {
        uint32_t end = tick_ms() + 50;
        while (log_head - log_tail >= LOG_SIZE)
            if (tick_after(tick_ms(), end)) {
                reader = false;
                break;
            }
    }
    if (log_head - log_tail >= LOG_SIZE) {
        irqflags_t f = cpu_irq_save();
        log_tail++;
        cpu_irq_restore(f);
    }
    logbuf[log_head % LOG_SIZE] = (uint8_t)c;
    log_head++;
#ifdef ZD1_EMU
    AVR32_USART0.thr = (uint8_t)c; /* emulator builds: the zd1 emulator prints USART0 (the run's mcu log) */
#endif
}
void log_puts(const char *s) {
    while (*s)
        log_putc(*s++);
}
void log_hexn(uint32_t v, int nib) {
    for (int i = (nib - 1) * 4; i >= 0; i -= 4) {
        int d = (v >> i) & 0xF;
        log_putc(d < 10 ? '0' + d : 'a' + d - 10);
    }
}
void log_hex8(uint8_t v) { log_hexn(v, 2); }
void log_dec(uint32_t v) {
    char b[11];
    int i = 10;
    b[i] = 0;
    do {
        b[--i] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    log_puts(b + i);
}
void log_ascii(const uint8_t *b, uint16_t n) {
    for (uint16_t i = 0; i < n && i < 64; i++)
        log_putc(b[i] >= 0x20 && b[i] < 0x7F ? (char)b[i] : '.');
    if (n > 64)
        log_puts("...");
}
