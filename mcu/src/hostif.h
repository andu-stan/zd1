/* The host link: typed requests from a computer on USB (docs/protocol/host.md, generated from protocol/host/src/lib.rs)
 * through a vendor interface on EP0, and the MCU's log, which the host reads with them. Everything the firmware
 * prints (log.h) goes into the log; the module's console goes over it too (`Console`, modcon.h). */
#ifndef HOSTIF_H
#define HOSTIF_H
#include <compiler.h>

#include "zd1host_gen.h" /* the protocol, generated: ZD1H_* (its USB interface: udi_hostif.h) */

void hostif_poll(uint32_t now); /* main loop: run a request that came in */
/* The gate (docs/protocol/host.md): a key event first goes here; true = it was the ON/OFF press an `Unlock` waits
 * for (and its release), taken: the source manager doesn't see it. */
bool hostif_key(uint8_t key);
uint8_t hostif_gate(uint16_t *secs_left); /* ZD1H_GATE_* */

/* the firmware's identification (version.c; the build stamps it: mcu/stamp.py) */
extern const char *const g_fw_version, *const g_fw_built, *const g_fw_board;

#endif
