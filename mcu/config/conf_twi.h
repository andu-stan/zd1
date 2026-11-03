#ifndef CONF_TWI_H
#define CONF_TWI_H
/* ASF avr32/drivers/twi config. TWI (PA29 SDA / PA30 SCL, 400 kHz in stock) talks to the ADAU1445 DSP 0x38,
 * AD193x codec 0x04, Apple auth 0x10 and the 24Cxx settings EEPROM 0x50 (docs/hardware/zd1-main-board.md). */
#define CONF_TWI_IRQ_LEVEL 1
#endif
