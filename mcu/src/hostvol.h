/* The computer's volume (USB audio's feature unit: -60..+10 dB in whole dB) and the speaker's volume steps (1..the
 * volume cap) map onto each other end to end, as the module maps its senders' 0..100 %: the whole of the computer's
 * slider does something, whatever the cap. Whole dB both ways, so a step read back gives the same step. Pure, for
 * mcu/test/hostvol_test.c. */
#ifndef HOSTVOL_H
#define HOSTVOL_H
#include <stdint.h>

#define HOSTVOL_DB_MIN  (-60)
#define HOSTVOL_DB_MAX  10
#define HOSTVOL_DB_SPAN (HOSTVOL_DB_MAX - HOSTVOL_DB_MIN)

/* the step for a host volume in dB, 1..cap */
static inline uint8_t hostvol_step(int db, uint8_t cap) {
    if (cap <= 1 || db <= HOSTVOL_DB_MIN)
        return 1;
    if (db >= HOSTVOL_DB_MAX)
        return cap;
    return (uint8_t)(1 + ((db - HOSTVOL_DB_MIN) * (cap - 1) + HOSTVOL_DB_SPAN / 2) / HOSTVOL_DB_SPAN);
}

/* the host volume in dB that reads back as this step (0 and 1: the bottom) */
static inline int hostvol_db(uint8_t step, uint8_t cap) {
    if (cap <= 1 || step <= 1)
        return HOSTVOL_DB_MIN;
    if (step >= cap)
        return HOSTVOL_DB_MAX;
    return HOSTVOL_DB_MIN + ((step - 1) * HOSTVOL_DB_SPAN + (cap - 1) / 2) / (cap - 1);
}
#endif
