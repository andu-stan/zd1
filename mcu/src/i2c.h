/* On-chip TWI master (PA29 SDA / PA30 SCL, 400 kHz) via ASF avr32/drivers/twi. Devices: ADAU1445 0x38,
 * AD193x 0x04, Apple auth 0x10, 24C02 settings EEPROM 0x50. Calls block until done (ASF is IRQ-driven). */
#ifndef I2C_H
#define I2C_H
#include <compiler.h>

#define I2C_DSP    0x38
#define I2C_CODEC  0x04
#define I2C_EEPROM 0x50

void i2c_init(void);
/* sub = internal (register) address, sublen 0..3 bytes. Return 0 = OK, <0 = ASF TWI error (e.g. -5/-7 NACK). */
int i2c_write(uint8_t chip, uint32_t sub, int sublen, const void *data, unsigned len);
int i2c_read(uint8_t chip, uint32_t sub, int sublen, void *data, unsigned len);

#endif
