/* The network module's console (its UART1: BL1, BL2, the app shell) on our USART1: PA06 TXD -> module RXD1 (J2 35),
 * module TXD1 (J2 37) -> PA05 RXD, 115200 8N1 ([HW 2026-10-23]; docs/hardware/wiring.md), passed
 * to the host link while a host has it open there (`Console`, `ConsoleWrite`: POLL's CONSOLE records), and otherwise
 * into our log a line at a time (the module's boot, its kernel log, panics: `zd1 log`). The MCU itself talks on it
 * only for BL1's escape (`Module` ESC) and while the module's updater has it (modcon_take: update.c). PA05/PA06 are the USART's only while the module is powered (modcon_power;
 * stock leaves them as plain inputs). */
#ifndef MODCON_H
#define MODCON_H
#include <compiler.h>

void modcon_poll(void);         /* main loop: the module's bytes to the host link or the log */
void modcon_power(bool on);     /* the module's supply on / off (dm870.c) */
uint32_t modcon_overruns(void); /* UART overrun / framing errors since boot */
/* BL1's escape into BL2 (Module ESC, dm870.c), started at the reset's release: 0x02 every 20 ms until BL1's menu is on
 * the console, then `4` CR once. Its result: -1 running (or not started), 1 BL1 starts BL2, 0 no menu or no start. */
void modcon_esc_start(void);
int modcon_esc_result(void);
/* the host link's console (hostif.c): opened / closed by `Console` (afresh: what came before is dropped) */
void modcon_host_open(bool open);
bool modcon_host(void);          /* open for the host link */
uint32_t modcon_host_lost(void); /* the module's bytes dropped since it last opened: POLL too slow */
unsigned modcon_host_take(uint8_t *out, unsigned n);    /* USB interrupt (POLL): up to n of the module's bytes */
unsigned modcon_host_avail(void);                       /* ... waiting */
unsigned modcon_host_put(const uint8_t *d, unsigned n); /* ConsoleWrite: to the module, as many as fit (returned) */
/* The module's updater (update.c) takes the line: its own baud, nothing passed on (either way), the rings empty.
 * modcon_give: back to 115200 and the passthrough. */
void modcon_take(uint32_t baud);
uint32_t modcon_baud(uint32_t baud); /* the baud the USART makes of that (0: too fast) */
void modcon_give(void);
int modcon_rx(void);                          /* taken: the next byte from the module, -1 = none */
unsigned modcon_tx_room(void);                /* the send ring's free bytes */
void modcon_tx(const uint8_t *d, unsigned n); /* at most modcon_tx_room() */

#endif
