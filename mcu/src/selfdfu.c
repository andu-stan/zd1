#include <udc.h>
#include <flashc.h>
#include <delay.h>
#include <wdt.h>
#include <pm.h>
#include "zd1_board.h"
#include "log.h"
#include "dm870.h"
#include "selfdfu.h"

/* ISP_FORCE = GP fuse bit 31 (EGPB -> 1, == stock): the DFU bootloader tests it first on ANY reset and stays in DFU.
 * The DFU start-app command clears it again (and any DFU DNLOAD sets it, so DFU sessions leave it set). */
#define ISP_FORCE_GP_FUSE_BIT 31
/* PM general-purpose low-power regs survive warm resets (the bootloader never touches them). GPLP0 bit24 =
 * stock's "came from the OTA DFU path" flag. */
#define GPLP_DFU_FLAG 0x01000000u
/* GPLP1 bit 0 (ours): the coming watchdog reset is one we asked for (selfdfu_enter, the reboot), not a trip */
#define GPLP1_OUR_RESET 0x00000001u

static bool isp_at_boot, wdt_tripped;

void selfdfu_capture_boot_state(void) {
    isp_at_boot = flashc_read_gp_fuse_bit(ISP_FORCE_GP_FUSE_BIT);
    uint32_t g = pm_read_gplp(&AVR32_PM, 1);
    wdt_tripped = (AVR32_PM.rcause & AVR32_PM_RCAUSE_WDT_MASK) && !(g & GPLP1_OUR_RESET);
    pm_write_gplp(&AVR32_PM, 1, g & ~GPLP1_OUR_RESET);
    wdt_disable(); /* a WDT still running from the reset (it survives its own) */
}

/* The running watchdog: ~2.3 s (PSEL 17, 2^18 cycles of the 115 kHz RC oscillator), fed once per main-loop pass;
 * stock's is ~1.1 s, kicked constantly. Every wait in the loop stays well below it (dm870.c: a read 300 ms, a write's
 * three tries ~1 s; the module's long waits run from the loop). A loop that stops (a hang) resets the MCU, the amp
 * going off with it (board_safe_gpio at the next boot), instead of leaving it on with the protections not looked
 * at. */
void selfdfu_watchdog_start(void) {
    wdt_opt_t opt = {.us_timeout_period = 2000000};
    wdt_enable(&opt);
}
void selfdfu_watchdog_feed(void) { wdt_clear(); }
bool selfdfu_watchdog_tripped(void) { return wdt_tripped; }

/* Safety net: clear a leftover ISP_FORCE so a stray power cycle can't loop into DFU. Normally dead code (the
 * bootloader's start-app paths clear it). Done after udc_start so a boot-time flash write can't stall USB. */
void selfdfu_after_usb_start(void) {
    if (isp_at_boot)
        flashc_write_gp_fuse_bit(ISP_FORCE_GP_FUSE_BIT, false);
}

/* Off USB, then a watchdog reset. NOT ASF wdt_reset_mcu(): it arms the minimum period (PSEL 0, ~17 us), and the WDT
 * survives its own reset, so the chip reset-loops before the bootloader can disable it (dark, off USB [HW
 * 2026-10-17]). Stock's ~1.1 s (PSEL 16) gives the bootloader time to disable it. */
static void watchdog_reset(void) {
    pm_write_gplp(&AVR32_PM, 1, pm_read_gplp(&AVR32_PM, 1) | GPLP1_OUR_RESET);
    udc_stop();
    cpu_irq_disable();
    wdt_opt_t opt = {.us_timeout_period = 1000000};
    wdt_enable(&opt);
    for (;;) {
    }
}

/* Self-DFU: module off, safe pins, ISP_FORCE, then a watchdog reset (= stock's reg-0x11=0x27 sequence) [HW]. */
void selfdfu_enter(void) {
    dm870_detach();
    board_safe_gpio();
    flashc_erase_gp_fuse_bit(ISP_FORCE_GP_FUSE_BIT, false); /* ISP_FORCE: EGPB bit31 == stock */
    pm_write_gplp(&AVR32_PM, 0, pm_read_gplp(&AVR32_PM, 0) | GPLP_DFU_FLAG);
    log_puts("ISP_FORCE set, detaching USB + watchdog reset; expect 2FF8 (DFU)\n");
    delay_ms(150); /* the log's last lines, if a host is reading */
    watchdog_reset();
}

/* A restart of this firmware: the module off and the pins safe, then the watchdog; ISP_FORCE stays clear, so the
 * bootloader starts the app again. */
void selfdfu_reboot(void) {
    dm870_detach();
    board_safe_gpio();
    log_puts("rebooting (watchdog reset)\n");
    delay_ms(100);
    watchdog_reset();
}
