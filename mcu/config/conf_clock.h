#ifndef CONF_CLOCK_H_INCLUDED
#define CONF_CLOCK_H_INCLUDED
/* Zeppelin Air MCU (AT32UC3A1256): OSC0 = 12 MHz crystal.
 * PLL0 = 12 * 8 / 2 = 48 MHz -> CPU/PBA/PBB = 48 MHz, and USB clock from PLL0 /1 = 48 MHz.
 * (Simpler than the stock 66 MHz; one PLL feeds both CPU and USB. Same math as EVK1100.) */

#define CONFIG_SYSCLK_SOURCE SYSCLK_SRC_PLL0

#define CONFIG_PLL0_SOURCE PLL_SRC_OSC0
#define CONFIG_PLL0_MUL    8 /* Fpll = (Fosc * MUL) / DIV = 12*8/2 = 48 MHz */
#define CONFIG_PLL0_DIV    2

/* CPU=PBA=PBB = Fsys/1 = 48 MHz (dividers left at default 0) */

#define CONFIG_USBCLK_SOURCE USBCLK_SRC_PLL0
#define CONFIG_USBCLK_DIV    1 /* 48 MHz / 1 = 48 MHz USB */

#endif /* CONF_CLOCK_H_INCLUDED */
