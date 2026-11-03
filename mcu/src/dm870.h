/* BridgeCo DM870 network module: power/reset sequencing and the SPI register protocol
 * (docs/protocol/spi-registers.md, decoded from stock 2.04.05). Powered at every boot like stock; notifications go to
 * wifi.c (the module as a source, over our OS's native registers). */
#ifndef DM870_H
#define DM870_H
#include <compiler.h>

void dm870_detach(void); /* stock detach: PA17 low, 20 ms, PA04 low; SPI pins -> inputs */
bool dm870_is_up(void);
bool dm870_powered(void); /* PA04 driven high */
void dm870_poll(void);    /* main loop: auto-drain notifications while PA20 is high */
void dm870_reg_write(uint8_t reg, uint8_t b0, uint8_t b1);
bool dm870_reg_write_v(uint8_t reg, const uint8_t *d, uint16_t n); /* V write (02 reg len data); false: no echo */
void dm870_start(void); /* non-blocking power-up + wait for ready (driven by dm870_poll) */
bool dm870_ready(void); /* status 0x2100/0x2300 seen */
/* reads: false = no answer in time. V: up to `max` bytes kept, *len = what the module sent */
bool dm870_reg_read(uint8_t reg, uint16_t *val);
bool dm870_reg_read_v(uint8_t reg, uint8_t *buf, uint16_t max, uint16_t *len);

/* the host link's view (hostif.c) */
uint8_t dm870_state(void);        /* ZD1H_MOD_STATE_* */
uint16_t dm870_last_status(void); /* the last reg 0x11 read */
bool dm870_ours(void);            /* up and running our module OS (reg 0x25 "zd1-os ...") */
bool dm870_native(void);   /* ... and it speaks this firmware's link protocol (reg 0xD5 = ZD1_NATIVE_PROTO; wifi.c) */
uint8_t dm870_proto(void); /* what reg 0xD5 said at ready (0: not ours, or nothing) */
bool dm870_usable(void);   /* a source, or may yet be: powered and not known to speak another protocol */
const char *dm870_why_unusable(void); /* for the host, when it isn't */
const char *dm870_os_version(void);   /* reg 0x25 at ready, "" unless ours */
const char *dm870_ip(void);           /* as the module notified (0x38), "" without one */
const char *dm870_name(void);         /* as the module notified (0x58) */
/* ZD1H_MOD_OP_*: power, bring-up, into BL2. POWER and BL2 run on from dm870_poll (the main loop goes on) for up to
 * timeout_s; dm870_op_result: -1 while one runs, then 1 = got there, 0 = didn't (the others end at once) */
void dm870_op(uint8_t op, uint32_t timeout_s);
int dm870_op_result(void);
/* Our OS's settings and records, queried over the configuration channel; the answer comes from the main loop:
 * dm870_query_result() -1 while it runs, then 1 = answered, 0 = not (up to 3 s). One at a time.
 * A setting: written first if `value`, then read back (false: too long, not our OS, the write not taken); its value
 * from dm870_setting_value once answered. */
bool dm870_setting_begin(const char *key, const char *value);
void dm870_setting_value(char *out, unsigned cap);
/* A record (the host link's `Record`), from `offset`: State (0) from the module's last push, others fetched from it
 * (once per read from offset 0). dm870_record_begin: 1 = it can be read now, -1 = being fetched, 0 = can't be.
 * dm870_record_read: *total its whole length, *n the bytes put in out[cap]. */
int dm870_record_begin(uint8_t rec, uint16_t offset);
void dm870_record_read(uint8_t rec, uint16_t offset, uint8_t *out, uint16_t cap, uint16_t *total, uint16_t *n);
int dm870_query_result(void);
/* XModem to BL2 over SPI (the host link's XmFrame): write d[n] to 0x2D (a frame, or a 1-byte control), wait up to
 * wait_ms (at most 500) for its answer on 0x2C; the times in µs. dm870_xm_take: the 0x2C bytes since the last take. */
void dm870_xm_send(const uint8_t *d, uint16_t n, uint16_t wait_ms, uint32_t *wire_us, uint32_t *wait_us,
                   uint32_t *read_us);
unsigned dm870_xm_take(uint8_t *out, unsigned cap);
void dm870_set_debug(uint8_t mode); /* ZD1H_DEBUG_*: kept in RAM, sent to our OS now and at every ready */
uint8_t dm870_debug(void);

#endif
