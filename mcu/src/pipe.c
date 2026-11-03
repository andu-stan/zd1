/* The bulk pipe (pipe.h): bulk OUT EP 0x05 into two 512-byte buffers. The USB interrupt fills one while the main loop
 * empties the other; with both full the endpoint isn't re-armed, so the host is NAKed: nothing is dropped. */
#include "conf_usb.h" /* first: the USB headers depend on it */
#include <string.h>
#include <udd.h>
#include "udi_hostif.h"
#include "pipe.h"

#define PIPE_BUF 512
COMPILER_WORD_ALIGNED static uint8_t pipe_buf[2][PIPE_BUF]; /* ASF's FIFO copy wants aligned buffers */
static volatile uint16_t pipe_len[2];                       /* bytes in a filled buffer; 0 = free */
static volatile int8_t pipe_rx = -1;                        /* the buffer the USB is filling, -1 = none */
static volatile uint32_t pipe_full;                         /* times both were full */
static volatile bool pipe_on;
static uint8_t pipe_head; /* the next buffer to empty */
static uint16_t pipe_off; /* sink MODULE: how much of it the consumer has taken */
static bool pipe_ep0;     /* PipeData fills the buffers (the emulator): the endpoint stays off */
static uint8_t pipe_sink;
static uint32_t pipe_bytes, pipe_crc = 0xFFFFFFFFu, pipe_packets;

uint32_t crc32_add(uint32_t c, const uint8_t *b, unsigned n) { /* zlib's, a nibble at a time */
    static const uint32_t t[16] = {0x00000000, 0x1DB71064, 0x3B6E20C8, 0x26D930AC, 0x76DC4190, 0x6B6B51F4,
                                   0x4DB26158, 0x5005713C, 0xEDB88320, 0xF00F9344, 0xD6D6A3E8, 0xCB61B38C,
                                   0x9B64C2B0, 0x86D3D2D4, 0xA00AE278, 0xBDBDF21C};
    while (n--) {
        c ^= *b++;
        c = (c >> 4) ^ t[c & 15];
        c = (c >> 4) ^ t[c & 15];
    }
    return c;
}

/* ---- the USB interrupt's side ---- */
static void pipe_arm(uint8_t i);
static void pipe_done(udd_ep_status_t st, iram_size_t n, udd_ep_id_t ep) {
    (void)ep;
    int8_t i = pipe_rx;
    pipe_rx = -1;
    if (st != UDD_EP_TRANSFER_OK || i < 0 || !pipe_on)
        return; /* aborted: the interface went down */
    if (!n) {   /* a zero-length packet: nothing to keep */
        pipe_arm((uint8_t)i);
        return;
    }
    pipe_len[i] = (uint16_t)n;
    if (!pipe_len[i ^ 1])
        pipe_arm((uint8_t)(i ^ 1));
    else
        pipe_full++;
}
static void pipe_arm(uint8_t i) {
    pipe_rx = (int8_t)i;
    if (!udd_ep_run(UDI_HOSTIF_EP_OUT, false, pipe_buf[i], PIPE_BUF, pipe_done))
        pipe_rx = -1;
}
void pipe_enable(void) {
    pipe_len[0] = pipe_len[1] = 0;
    pipe_head = 0;
    pipe_on = true;
    pipe_arm(0);
}
void pipe_disable(void) { pipe_on = false; }

/* ---- the main loop's side ---- */
/* the head buffer emptied: the next one, a stopped endpoint re-armed */
static void pipe_free_head(void) {
    irqflags_t f = cpu_irq_save();
    pipe_len[pipe_head] = 0;
    if (pipe_on && pipe_rx < 0 && !pipe_ep0)
        pipe_arm(pipe_head);
    cpu_irq_restore(f);
    pipe_head ^= 1;
    pipe_off = 0;
}
/* DISCARD empties the filled buffers in order, counting and checking them; MODULE waits for its consumer */
void pipe_poll(void) {
    while (pipe_sink == ZD1H_SINK_DISCARD && pipe_len[pipe_head]) {
        uint16_t n = pipe_len[pipe_head];
        pipe_crc = crc32_add(pipe_crc, pipe_buf[pipe_head], n);
        pipe_bytes += n;
        pipe_packets++;
        pipe_free_head();
    }
}
uint16_t pipe_peek(const uint8_t **d) {
    uint16_t n = pipe_len[pipe_head];
    if (pipe_sink != ZD1H_SINK_MODULE || n <= pipe_off)
        return 0;
    *d = pipe_buf[pipe_head] + pipe_off;
    return (uint16_t)(n - pipe_off);
}
void pipe_take(uint16_t k) {
    pipe_crc = crc32_add(pipe_crc, pipe_buf[pipe_head] + pipe_off, k);
    pipe_bytes += k;
    pipe_off += k;
    if (pipe_off >= pipe_len[pipe_head]) {
        pipe_packets++;
        pipe_free_head();
    }
}

/* a clean start: the transfer in progress (a partial 512, if the host's last one ended on a full packet) dropped */
static void pipe_restart(void) {
    pipe_on = false;
    udd_ep_abort(UDI_HOSTIF_EP_OUT);
    pipe_rx = -1;
    pipe_len[0] = pipe_len[1] = 0;
    pipe_head = 0;
    pipe_off = 0;
    pipe_ep0 = false;
    pipe_bytes = pipe_packets = 0;
    pipe_crc = 0xFFFFFFFFu;
    pipe_full = 0;
    pipe_on = true;
    pipe_arm(0);
}
void pipe_to(uint8_t sink) {
    pipe_restart();
    pipe_sink = sink;
}
uint8_t pipe_sink_now(void) { return pipe_sink; }

void pipe_request(uint8_t sink, bool reset, bool keep_sink, zd1h_pipe_reply_t *r) {
    pipe_poll();
    if (reset)
        pipe_restart();
    if (!keep_sink)
        pipe_sink = sink;
    r->sink = pipe_sink;
    r->bytes = pipe_bytes;
    r->crc32 = ~pipe_crc;
    r->packets = pipe_packets;
    r->full = pipe_full;
}

#ifdef ZD1_EMU
bool pipe_put(const uint8_t *d, uint16_t n) {
    if (!pipe_ep0) { /* from now on PipeData fills the buffers, the endpoint is off */
        pipe_on = false;
        udd_ep_abort(UDI_HOSTIF_EP_OUT);
        pipe_rx = -1;
        pipe_ep0 = true;
        pipe_on = true;
    }
    uint8_t t = pipe_len[pipe_head] ? pipe_head ^ 1 : pipe_head;
    if (pipe_len[t] || !n || n > PIPE_BUF)
        return false;
    memcpy(pipe_buf[t], d, n);
    pipe_len[t] = n;
    return true;
}
#endif
