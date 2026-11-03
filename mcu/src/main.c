/* ZD1 (B&W Zeppelin Air) main MCU firmware, ASF-based.
 * Boot order mirrors the bootloader/stock (pins first on the RC clock, then clocks), then USB.
 * Modules: zd1_board (pins/clocks/boot log), log (what the firmware prints), selfdfu (DFU from the app),
 * dm870 (network module power + SPI protocol), audio (DSP/codec/CS2000/amp), settings (EEPROM), inputs (pins + keys),
 * led (PWM patterns), source (source manager + standby/run state machine), USB audio (udi_audio + udi_hid_cc classes,
 * usbaudio streaming engine), hostif (the host link: typed requests and the log over a vendor interface), modcon (the
 * module's console, to the host link or the log). The USB device is composite: UAC1 + HID + vendor. */
#include <string.h>
#include <sysclk.h>
#include <udc.h>
#include <delay.h>
#include "main.h"
#include "zd1_board.h"
#include "log.h"
#include "selfdfu.h"
#include "dm870.h"
#include "update.h"
#include "audio.h"
#include "settings.h"
#include "inputs.h"
#include "led.h"
#include "amp.h"
#include "source.h"
#include "tick.h"
#include "ir.h"
#include "protect.h"
#include "usbaudio.h"
#include "udi_audio.h"
#include "wifi.h"
#include "modcon.h"
#include "hostif.h"
#include <intc.h>
#include <pm.h>

/* Between main-loop passes the CPU sleeps (idle mode: clocks and peripherals keep running) until the next
 * interrupt or the 1 kHz wake tick (TC0, RC compare, from PBA / 128; the CPU's COMPARE would reset COUNT, which the
 * ms tick, IR timing and cpu_delay all read). Not from the 32 kHz oscillator, TIMER_CLOCK1: nothing starts it, and
 * TC0 doesn't count on it [HW 2026-10-27]; without a wake tick the loop runs only when another interrupt comes (the
 * USB's 1 ms SOF, gone with the cable out). Everything here is interrupt-driven (USB, IR edges, audio DMA) or polled
 * on a ms scale. Saves power on the unit, and the emulators skip the idle time instead of spinning through. */
ISR(wake_isr, AVR32_TC_IRQ_GROUP, 0) {
    (void)AVR32_TC.channel[0].sr; /* read clears CPCS */
    tick_ms();                    /* the ms clock never misses a cycle-counter wrap (tick.h) */
}

static void wake_tick_init(void) {
    volatile avr32_tc_channel_t *ch = &AVR32_TC.channel[0];
    sysclk_enable_pba_module(SYSCLK_TC);
    ch->cmr = AVR32_TC_WAVE_MASK | (2 << AVR32_TC_WAVSEL_OFFSET) | 4; /* waveform, up to RC; TCCLKS 4 = PBA / 128 */
    ch->rc = (uint16_t)(sysclk_get_pba_hz() / 128 / 1000);            /* 375 at 48 MHz: 1 kHz */
    ch->ier = AVR32_TC_CPCS_MASK;
    irq_register_handler(wake_isr, AVR32_TC_IRQ0, 0);
    ch->ccr = AVR32_TC_CLKEN_MASK | AVR32_TC_SWTRG_MASK;
}

static inline void idle(void) { SLEEP(AVR32_PM_SMODE_IDLE); }

int main(void) {
    board_bl_pins(); /* bootloader's pin state, on the reset RC clock */
    /* the red LED off (lit here it would flash at every power-on); a clock that won't start blinks it (below) */
    gpio_configure_pin(PIN_LED_R, GPIO_DIR_OUTPUT | GPIO_INIT_LOW);
    g_rcause = AVR32_PM.rcause;
    blog(1);
    if (!board_clock_init()) { /* blink = clock fail; still on the RC oscillator */
        for (;;) {
            gpio_toggle_pin(PIN_LED_R);
            cpu_delay_ms(250, AVR32_PM_RCOSC_FREQUENCY);
        }
    }
    irq_initialize_vectors();
    cpu_irq_enable();
    board_safe_gpio();
    selfdfu_capture_boot_state();
    board_usb_mux_device();
    blog(6);
    udc_start();
    blog(7);
    selfdfu_after_usb_start();

    led_init();
    led_poll(tick_ms()); /* the boot look (breathing red) from the start */
    audio_init();        /* CS2000, DSP download, codec; amp stays off */
    settings_load();
    inputs_init();
    ir_init();
    protect_init();
    dm870_start(); /* stock powers the network module at every boot (non-blocking) */
    source_init(); /* STANDBY or RUN(best present source); amp allowed unless `amp deny` locked it (EEPROM) */

    wake_tick_init();
    blog(8);
    if (selfdfu_watchdog_tripped())
        log_puts("[boot] the watchdog reset the MCU: its main loop had stopped\n");
    selfdfu_watchdog_start();
    for (;;) {
        selfdfu_watchdog_feed();
        uint32_t now = tick_ms();
        inputs_poll(now);
        ir_poll(now);
        source_poll(now);
        usbaudio_poll(now);
        protect_poll(now);
        amp_poll(now);
        led_poll(now);
        settings_poll(now);
        dm870_poll();
        hostif_poll(now);
        update_poll(now);
        modcon_poll();
        idle();
    }
}

/* ---- ASF USB callbacks (conf_usb.h) ---- */
void main_suspend_action(void) { udi_audio_note(UAC_EV_SUSPEND, 0); }
void main_resume_action(void) { udi_audio_note(UAC_EV_RESUME, 0); }
