#include <sysclk.h>
#include <gpio.h>
#include <delay.h>
#include "zd1_board.h"
#include "cs2000.h"

/* Bit-bang like stock/bootloader (bl.dis 0x800011a8..): SCL is driven push-pull, SDA is pulled low or released
 * (input, external pull-up). ~2 us half-bit = ~250 kHz, close to stock's 114-cycle waits at 66 MHz. */
#define HALF() delay_us(2)
static void scl(bool h) {
    if (h)
        gpio_set_pin_high(PIN_CS2000_SCL);
    else
        gpio_set_pin_low(PIN_CS2000_SCL);
}
static void sda(bool h) {
    if (h)
        gpio_configure_pin(PIN_CS2000_SDA, GPIO_DIR_INPUT);
    else
        gpio_configure_pin(PIN_CS2000_SDA, GPIO_DIR_OUTPUT | GPIO_INIT_LOW);
}
static void start(void) {
    sda(1);
    scl(1);
    HALF();
    sda(0);
    HALF();
    scl(0);
    HALF();
}
static void stop(void) {
    sda(0);
    HALF();
    scl(1);
    HALF();
    sda(1);
    HALF();
}
static bool byte_out(uint8_t b) { /* returns true on ACK */
    for (int i = 7; i >= 0; i--) {
        sda((b >> i) & 1);
        HALF();
        scl(1);
        HALF();
        scl(0);
    }
    sda(1);
    HALF();
    scl(1);
    HALF();
    bool ack = !gpio_get_pin_value(PIN_CS2000_SDA);
    scl(0);
    HALF();
    return ack;
}
/* one register write: [0x9C][reg | 0x80 (auto-increment)][data...] */
static int wr(uint8_t reg, const uint8_t *d, int n) {
    int nak = 0;
    start();
    nak += !byte_out(0x9C);
    nak += !byte_out(0x80 | reg);
    for (int i = 0; i < n; i++)
        nak += !byte_out(d[i]);
    stop();
    return nak;
}
static int wr1(uint8_t reg, uint8_t v) { return wr(reg, &v, 1); }

void cs2000_refclk_on(void) {
    /* stock (zd1 emulator trace): OSCCTRL1 = 0x300 (external clock, startup 3), OSC1EN, wait OSC1RDY,
     * GCCTRL0 = 0x005 (OSC1, on), GCCTRL2 = 0x115 (OSC1 / 4, on), PB30 -> function B (GCLK2 out). */
    AVR32_PM.oscctrl1 = 0x00000300;
    AVR32_PM.mcctrl |= AVR32_PM_MCCTRL_OSC1EN_MASK;
    for (uint32_t t = 0; !(AVR32_PM.poscsr & AVR32_PM_POSCSR_OSC1RDY_MASK) && t < 2000000u; t++) {
    }
    struct genclk_config g;
    genclk_config_defaults(&g, 0);
    genclk_config_set_source(&g, GENCLK_SRC_OSC1);
    genclk_enable(&g, 0);
    genclk_config_defaults(&g, 2);
    genclk_config_set_source(&g, GENCLK_SRC_OSC1);
    genclk_config_set_divider(&g, 4);
    genclk_enable(&g, 2);
    gpio_enable_module_pin(AVR32_PM_GCLK_2_1_PIN, AVR32_PM_GCLK_2_1_FUNCTION);
}

/* Stock order (decoded from both the bootloader and the app in the zd1 emulator):
 * 05<-08, 02<-00, 03<-00, 06..09<-00 0F 0D 84 (44.1 kHz ratio), 16<-10, 17<-00, 05<-01, 00<-99, 77<-80. */
int cs2000_init(void) {
    static const uint8_t ratio441[4] = {0x00, 0x0F, 0x0D, 0x84};
    gpio_configure_pin(PIN_CS2000_SCL, GPIO_DIR_OUTPUT | GPIO_INIT_HIGH);
    sda(1);
    int nak = 0;
    nak += wr1(0x05, 0x08);
    nak += wr1(0x02, 0x00);
    nak += wr1(0x03, 0x00);
    nak += wr(0x06, ratio441, 4);
    nak += wr1(0x16, 0x10);
    nak += wr1(0x17, 0x00);
    nak += wr1(0x05, 0x01);
    nak += wr1(0x00, 0x99);
    nak += wr1(0x77, 0x80);
    return nak;
}

/* Ratio = 256 fs / 12 MHz in 12.20 fixed point (stock values for 32k / 44.1k / 48k). One LSB is ~0.9 ppm, so the USB
 * audio rate trim (usbaudio.c) writes nudged ratios with cs2000_set_ratio(). */
uint32_t cs2000_ratio_for(uint32_t fs) { return fs == 32000 ? 0x000AEC33 : fs == 48000 ? 0x0010624D : 0x000F0D84; }
int cs2000_set_ratio(uint32_t r) {
    uint8_t b[4] = {r >> 24, r >> 16, r >> 8, r};
    return wr(0x06, b, 4);
}
int cs2000_set_rate(uint32_t fs) { return cs2000_set_ratio(cs2000_ratio_for(fs)); }
