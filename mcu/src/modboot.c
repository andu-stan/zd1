#include "zd1link.h"
#include "zd1host_gen.h"
#include "settings.h"
#include "log.h"
#include "modboot.h"

/* How long a boot may take to get ready: a slot ~20 s on the unit; the rescue may install the app partition's system
 * into a slot first (~60 s) */
#define SLOT_READY_MS   90000u
#define RESCUE_READY_MS 180000u

static uint8_t last;   /* ZD1_BOOT_* answered at the module's last boot, 0 = none */
static bool pending;   /* ... and it hasn't got ready yet */
static uint8_t failed; /* bit (slot - 1): didn't get ready since the last ready */

uint8_t modboot_good(void) { return g_settings.flags & SETTINGS_BOOT_GOOD_B ? ZD1_BOOT_SLOT_B : ZD1_BOOT_SLOT_A; }
uint8_t modboot_last(void) { return last; }
static uint8_t next(void) { return (g_settings.flags & SETTINGS_BOOT_TRY_MASK) >> SETTINGS_BOOT_TRY_SHIFT; }
static void put(uint8_t good_slot, uint8_t try_slot) {
    uint8_t f = g_settings.flags & ~(SETTINGS_BOOT_GOOD_B | SETTINGS_BOOT_TRY_MASK);
    if (good_slot == ZD1_BOOT_SLOT_B)
        f |= SETTINGS_BOOT_GOOD_B;
    f |= (uint8_t)(try_slot << SETTINGS_BOOT_TRY_SHIFT) & SETTINGS_BOOT_TRY_MASK;
    if (f == g_settings.flags)
        return;
    g_settings.flags = f;
    settings_save_now(); /* at once: the module may be power-cycled next */
}
static const char *name(uint8_t s) {
    return s == ZD1_BOOT_SLOT_A ? "slot A" : s == ZD1_BOOT_SLOT_B ? "slot B" : "the rescue";
}
static bool skip(uint8_t s) { return failed & (1u << (s - 1)); }
static uint8_t other(uint8_t s) { return s == ZD1_BOOT_SLOT_A ? ZD1_BOOT_SLOT_B : ZD1_BOOT_SLOT_A; }

uint16_t modboot_answer(void) {
    if (pending) {
        failed |= (uint8_t)(1u << (last - 1));
        log_puts("[boot] ");
        log_puts(name(last));
        log_puts(" didn't get ready: it booted again\n");
    }
    uint8_t g = modboot_good(), t = next(), a;
    if (t != ZD1H_SLOT_NONE && !skip(t)) {
        a = t;
        put(g, ZD1H_SLOT_NONE);
    } /* once */
    else if (!skip(g))
        a = g;
    else if (!skip(other(g)))
        a = other(g);
    else
        a = ZD1_BOOT_RESCUE;
    last = a;
    pending = true;
    log_puts("[boot] starting ");
    log_puts(name(a));
    log_puts(a == g                 ? "\n"
             : a == ZD1_BOOT_RESCUE ? " (no slot got ready)\n"
             : t == a               ? " (a try: once)\n"
                                    : " (the good one didn't get ready)\n");
    return (uint16_t)(a << 8 | g);
}

void modboot_ready(void) {
    if (!pending)
        return;
    pending = false;
    failed = 0;
    if (last != ZD1_BOOT_RESCUE && last != modboot_good()) {
        put(last, next());
        log_puts("[boot] ");
        log_puts(name(last));
        log_puts(" got ready: the good slot now\n");
    }
}

bool modboot_failed(void) {
    if (!pending)
        return false;
    pending = false;
    failed |= (uint8_t)(1u << (last - 1));
    log_puts("[boot] ");
    log_puts(name(last));
    log_puts(" didn't get ready in time\n");
    return last != ZD1_BOOT_RESCUE;
}

void modboot_cancel(void) { pending = false; }

uint32_t modboot_deadline_ms(void) {
    if (!pending)
        return 0;
    return last == ZD1_BOOT_RESCUE ? RESCUE_READY_MS : SLOT_READY_MS;
}

bool modboot_set(uint8_t op, uint8_t slot) {
    switch (op) {
    case ZD1H_BOOT_OP_TRY:
        if (slot < ZD1H_SLOT_A || slot > ZD1H_SLOT_RESCUE)
            return false;
        put(modboot_good(), slot);
        failed &= (uint8_t)~(1u << (slot - 1));
        log_puts("[boot] next boot: ");
        log_puts(name(slot));
        log_puts(", once\n");
        return true;
    case ZD1H_BOOT_OP_GOOD:
        if (slot != ZD1H_SLOT_A && slot != ZD1H_SLOT_B)
            return false;
        put(slot, next());
        log_puts("[boot] good slot: ");
        log_puts(name(slot));
        log_putc('\n');
        return true;
    case ZD1H_BOOT_OP_CLEAR:
        put(modboot_good(), ZD1H_SLOT_NONE);
        failed = 0;
        log_puts("[boot] no try, no failures\n");
        return true;
    default:
        return false;
    }
}

void modboot_state(uint8_t *g, uint8_t *t, uint8_t *l, bool *trial, uint8_t *f) {
    *g = modboot_good();
    *t = next();
    *l = last;
    *trial = pending;
    *f = failed;
}
