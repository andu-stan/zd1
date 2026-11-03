#include <delay.h>
#include <gpio.h>
#include "zd1_board.h"
#include "i2c.h"
#include "codec.h"

static int wr(uint8_t reg, uint8_t v) { return i2c_write(I2C_CODEC, reg, 1, &v, 1); }
static int codec_read(uint8_t reg, uint8_t *v) { return i2c_read(I2C_CODEC, reg, 1, v, 1); }

/* Stock FUN_80020db0 (docs/mcu/stock-firmware.md, "Codec"): reg0 = 0x19, 20 ms, reg0 = 0x18, wait for PLL lock (reg1
 * bit3, re-kicking if it does not come), then reg2 = 0x02, reg4 = 0x01, reg5 = 0xC0 (DAC4 L/R muted), reg14 = 0x7E,
 * reg0 = 0x98. The lock takes longer than 25 ms after a kick (re-kicking every 25 ms never locks [HW 2026-10-17]), so
 * each kick polls up to 100 ms. Stock retries forever; we give up after 10 kicks. */
static unsigned lock_ms;
unsigned codec_lock_ms(void) { return lock_ms; }
/* PB27 = codec mute, active HIGH: stock (emulator trace) drives it LOW from boot, HIGH around the codec set-up,
 * LOW again after. Left floating it reads high: muted, no sound on any source [HW 2026-10-18]. */
static int init_regs(void);
int codec_init(void) {
    gpio_configure_pin(PIN_CODEC_MUTE, GPIO_DIR_OUTPUT | GPIO_INIT_HIGH);
    int rc = init_regs();
    gpio_set_pin_low(PIN_CODEC_MUTE);
    return rc;
}
static int init_regs(void) {
    int rc;
    uint8_t st = 0;
    lock_ms = 0;
    for (int kick = 0;; kick++) {
        if (kick == 10)
            return -100; /* PLL never locked */
        if ((rc = wr(0x00, 0x19)))
            return rc;
        delay_ms(20);
        if ((rc = wr(0x00, 0x18)))
            return rc;
        lock_ms += 20;
        for (int t = 0; t < 20; t++) {
            delay_ms(5);
            lock_ms += 5;
            if ((rc = codec_read(0x01, &st)))
                return rc;
            if (st & 0x08)
                goto locked;
        }
    }
locked:
    if ((rc = wr(0x02, 0x02)) || (rc = wr(0x04, 0x01)) || (rc = wr(0x05, 0xC0)) || (rc = wr(0x0E, 0x7E)))
        return rc;
    return wr(0x00, 0x98);
}

/* DAC master mute = reg 4 (DAC Control 2) bit 0. The init sequence leaves it SET (stock does the same); stock clears
 * it only once the amp is unmuted and the source is ready (FUN_80013a68 -> FUN_80020d68: reg4 = 0) and sets it again
 * on every source switch / standby (FUN_800138f0 -> FUN_80020d8c: reg4 = 1). Left set, no source plays (the mic
 * probe: reg4 0x01 silent, 0x00 plays [HW 2026-10-18]). */
int codec_dac_mute(bool mute) { return wr(0x04, mute ? 0x01 : 0x00); }

/* AUX analog extra (stock FUN_80020d20, gated on a flag): reg 0x0E = 0x72. */
int codec_aux_mode(void) { return wr(0x0E, 0x72); }
