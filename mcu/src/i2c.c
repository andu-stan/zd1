#include <sysclk.h>
#include <gpio.h>
#include <delay.h>
#include <twi.h>
#include "zd1_board.h"
#include "i2c.h"

/* The ADAU1445 must be powered (PB03) before the bus is used: unpowered, it is believed to clamp SDA/SCL, and a
 * transfer never completes (an I2C scan with it off hung the MCU [HW 2026-10-17]; ASF's waits are bounded here: the
 * patch in mcu/README.md). Stock also drives PB03 high early at boot. The DSP stays in reset (PB02) until its
 * download. */
void i2c_init(void) {
    if (!gpio_get_gpio_pin_output_value(PIN_DSP_PWR)) {
        gpio_set_pin_high(PIN_DSP_PWR);
        delay_ms(10);
    }
    static const gpio_map_t pins = {
        {AVR32_TWI_SDA_0_0_PIN, AVR32_TWI_SDA_0_0_FUNCTION}, /* PA29 */
        {AVR32_TWI_SCL_0_0_PIN, AVR32_TWI_SCL_0_0_FUNCTION}, /* PA30 */
    };
    const twi_options_t opt = {.pba_hz = sysclk_get_pba_hz(), .speed = 400000, .chip = 0};
    gpio_enable_module(pins, sizeof pins / sizeof pins[0]);
    sysclk_enable_pba_module(SYSCLK_TWI);
    twi_master_init(&AVR32_TWI, &opt);
}

static void pkg(twi_package_t *p, uint8_t chip, uint32_t sub, int sublen, void *data, unsigned len) {
    p->chip = chip;
    p->addr_length = sublen;
    for (int i = 0; i < sublen; i++)
        p->addr[i] = (uint8_t)(sub >> (8 * (sublen - 1 - i))); /* MSB first */
    p->buffer = data;
    p->length = len;
}

int i2c_write(uint8_t chip, uint32_t sub, int sublen, const void *data, unsigned len) {
    twi_package_t p;
    if (len == 0) {   /* address-only write (e.g. DSP download step 19): ASF rejects length 0, */
        uint8_t a[3]; /* so send the sub-address bytes as data; identical on the wire */
        for (int i = 0; i < sublen; i++)
            a[i] = (uint8_t)(sub >> (8 * (sublen - 1 - i)));
        pkg(&p, chip, 0, 0, a, sublen);
        return twi_master_write(&AVR32_TWI, &p);
    }
    pkg(&p, chip, sub, sublen, (void *)data, len);
    return twi_master_write(&AVR32_TWI, &p);
}

int i2c_read(uint8_t chip, uint32_t sub, int sublen, void *data, unsigned len) {
    twi_package_t p;
    pkg(&p, chip, sub, sublen, data, len);
    return twi_master_read(&AVR32_TWI, &p);
}
