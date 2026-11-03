/* Which system slot the network module starts (docs/module/storage.md, The boot). Its boot stub asks at every boot
 * (it says "zd1-boot" on reg 0x25 with status booting); dm870.c answers on reg 0xC4 with modboot_answer(). The choice
 * is ours: the good slot and a slot to try once live in the EEPROM (settings flags), the failures in RAM. A boot that
 * doesn't get ready counts as failed (the stub asking again before a ready, or dm870.c's deadline power-cycling it);
 * the next answer skips the failed slots: the good one, the other one, then the rescue. A ready clears the failures
 * and makes a tried slot the good one. */
#ifndef MODBOOT_H
#define MODBOOT_H
#include <stdint.h>
#include <stdbool.h>

uint16_t modboot_answer(void);      /* reg 0xC4: b0 = ZD1_BOOT_* to start, b1 = the good slot */
void modboot_ready(void);           /* the module reported ready (0x2100) */
bool modboot_failed(void);          /* the boot just answered didn't get ready in time; true: try another */
void modboot_cancel(void);          /* the module was powered off on purpose: that boot doesn't count */
uint32_t modboot_deadline_ms(void); /* how long the boot just answered may take to get ready (0: none answered) */
bool modboot_set(uint8_t op, uint8_t slot); /* ZD1H_BOOT_OP_*; false = a bad slot for it */
void modboot_state(uint8_t *good, uint8_t *next, uint8_t *last, bool *trial, uint8_t *failed);
uint8_t modboot_good(void); /* the good slot (ZD1_BOOT_SLOT_*) */
uint8_t modboot_last(void); /* what the module's last boot was told to start (0: none yet) */

#endif
