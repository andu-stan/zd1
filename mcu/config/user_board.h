#ifndef USER_BOARD_H
#define USER_BOARD_H
/* Minimal USER_BOARD for the Zeppelin Air MCU. No LED/joystick pins declared; the
 * ASF clock/USB drivers don't need board pin macros. OSC0 is a 12 MHz crystal. */
#include "compiler.h"

#define BOARD_OSC0_HZ         12000000
#define BOARD_OSC0_STARTUP_US 17000
#define BOARD_OSC0_IS_XTAL    true

/* OSC1 is an external 12 MHz clock, the CS2000's reference (cs2000.c starts it); ASF's clock config doesn't use it */

#endif /* USER_BOARD_H */
