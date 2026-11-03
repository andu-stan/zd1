/* The host link's bulk pipe (pipe.c; docs/protocol/host.md, Pipe): the host's bulk data on EP 0x05, for a consumer
 * (sink MODULE: update.c) or counted and checked (DISCARD, the host's throughput test). */
#ifndef PIPE_H
#define PIPE_H
#include <compiler.h>
#include "zd1host_gen.h"

void pipe_enable(void);  /* the interface up (USB interrupt): the endpoint armed */
void pipe_disable(void); /* ... down */
void pipe_poll(void);    /* main loop: DISCARD's buffers emptied */
/* the data for a consumer: restart it from zero into the sink; the bytes at its head (0 = none yet) and taking them */
void pipe_to(uint8_t sink);
uint16_t pipe_peek(const uint8_t **d);
void pipe_take(uint16_t n);
uint8_t pipe_sink_now(void);
/* the host's Pipe: restart if reset, the sink set unless keep_sink (an update owns it), the counters into r */
void pipe_request(uint8_t sink, bool reset, bool keep_sink, zd1h_pipe_reply_t *r);
#ifdef ZD1_EMU
bool pipe_put(const uint8_t *d, uint16_t n); /* PipeData (the emulator): false = both buffers full */
#endif
/* zlib's CRC-32, running: start from 0xFFFFFFFF, the result is its complement */
uint32_t crc32_add(uint32_t c, const uint8_t *b, unsigned n);

#endif
