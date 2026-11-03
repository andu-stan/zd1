/* IR remote: Philips RC6 mode 0 on PA03 (receiver output, active low), T = 444 us, address 0xD2
 * (docs/hardware/zd1-main-board.md, "LED, keys, IR"). Edge-timestamped in a GPIO interrupt, decoded in the main loop, delivered as key events. */
#ifndef IR_H
#define IR_H
#include <stdint.h>
#include <stdbool.h>

#define IR_ADDR 0xD2
void ir_init(void);
void ir_poll(uint32_t now);
/* the last decoded frame (false: none yet) and how many since boot */
bool ir_last(uint8_t *addr, uint8_t *cmd, bool *toggle, uint32_t *at_ms, uint16_t *count);

#endif
