/* The firmware's log: everything it prints (progress, events, the network module's console: modcon.c), in a 4 KB
 * ring the host reads over USB with the host link's POLL (hostif.c); emulator builds (ZD1_EMU) mirror it on USART0. */
#ifndef LOG_H
#define LOG_H
#include <compiler.h>

void log_putc(char c);
void log_puts(const char *s);
void log_hexn(uint32_t v, int nibbles);
void log_hex8(uint8_t v);
void log_dec(uint32_t v);
void log_ascii(const uint8_t *b, uint16_t n); /* printable chars, '.' otherwise, max 64 */

/* the reader's side (the host link) */
void log_reader(bool present);                 /* a host polls: a full log waits up to 50 ms for it */
unsigned log_take(uint8_t *out, unsigned cap); /* the oldest text, taken (the USB interrupt) */
uint32_t log_written(void);                    /* bytes written since boot, free-running */
uint32_t log_taken(void);                      /* ... and taken */

#endif
