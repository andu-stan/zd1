#include <sysclk.h>
#include <cycle_counter.h>
#include <interrupt.h>
#include "tick.h"

uint32_t tick_ms(void) {
    static uint32_t last_cy, ms, rem_cy;
    irqflags_t f = cpu_irq_save(); /* the main loop and the wake interrupt both call it */
    uint32_t now = Get_sys_count(), per_ms = sysclk_get_cpu_hz() / 1000u;
    rem_cy += now - last_cy; /* wrap-safe delta */
    last_cy = now;
    ms += rem_cy / per_ms;
    rem_cy %= per_ms;
    uint32_t r = ms;
    cpu_irq_restore(f);
    return r;
}
