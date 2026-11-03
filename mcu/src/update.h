/* Updating the network module's system (docs/module/updater.md): the host's image from the bulk pipe to the module's
 * updater (zd1-update) over its console UART, as fast as the module grants it. */
#ifndef UPDATE_H
#define UPDATE_H
#include <stdint.h>
#include <stdbool.h>
#include "zd1host_gen.h"

const char *update_begin(const zd1h_update_t *q); /* NULL = started, else why not */
void update_abort(const char *why);               /* a running update fails, for this reason */
bool update_busy(void);                           /* started and not ended */
void update_state(zd1h_update_reply_t *r);
void update_poll(uint32_t now); /* main loop */

#endif
