/* Self-DFU: re-enter the factory USB-DFU bootloader from the app (ISP_FORCE fuse + watchdog reset), plus the
 * boot-time clean-up that goes with it (docs/mcu/bootloader-dfu.md). */
#ifndef SELFDFU_H
#define SELFDFU_H
#include <compiler.h>

void selfdfu_capture_boot_state(void); /* call early: ISP_FORCE at boot; disables a running WDT */
void selfdfu_after_usb_start(void);    /* clears a leftover ISP_FORCE (bounded) */

void selfdfu_enter(void);  /* into DFU: ISP_FORCE + watchdog reset (no return) */
void selfdfu_reboot(void); /* restart the app (watchdog reset, ISP_FORCE clear); no return */

void selfdfu_watchdog_start(void);   /* the running watchdog (~2.3 s): before the main loop */
void selfdfu_watchdog_feed(void);    /* once per main-loop pass */
bool selfdfu_watchdog_tripped(void); /* this boot came from a watchdog trip (not a reset we asked for) */

#endif
