/* Monotonic milliseconds from the CPU cycle counter. COUNT wraps every 89.5 s at 48 MHz, so tick_ms() must run at
 * least that often: the 1 kHz wake interrupt calls it (main.c), so a main loop blocked in a long request loses no
 * time. Interrupt-safe. Every deadline is on it: ASF's cpu_set_timeout() counts cycles in 32 bits, so a wait of over
 * ~89 s wraps short. */
#ifndef TICK_H
#define TICK_H
#include <stdint.h>
#include <stdbool.h>

uint32_t tick_ms(void);
static inline bool tick_after(uint32_t now, uint32_t t) { return (int32_t)(now - t) >= 0; } /* now >= t, wrap-safe */

#endif
