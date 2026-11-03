#include <gpio.h>
#include "zd1_board.h"
#include "log.h"
#include "i2c.h"
#include "dsp.h"
#include "codec.h"
#include "cs2000.h"
#include "amp.h"
#include "audio.h"

static bool i2c_up;
static void ensure_i2c(void) {
    if (!i2c_up) {
        i2c_init();
        i2c_up = true;
    }
}
static void rc_line(const char *what, int rc) {
    log_puts("  ");
    log_puts(what);
    log_puts(rc == 0 ? ": ok\n" : ": FAILED rc=");
    if (rc) {
        if (rc < 0) {
            log_putc('-');
            log_dec((uint32_t)-rc);
        } else
            log_dec((uint32_t)rc);
        log_putc('\n');
    }
}

int audio_init(void) {
    int rc, worst = 0;
    amp_off(); /* never live during bring-up */
    ensure_i2c();
    cs2000_refclk_on(); /* OSC1 -> GCLK2 12 MHz ref (stock does this in clock init) */
    /* CS2000 first: it makes the codec's MCLK, without which the codec's PLL doesn't lock [HW 2026-10-17]; stock's
     * trace programs the CS2000 before any TWI traffic too */
    rc = cs2000_init();
    rc_line("CS2000 (NAKs)", rc);
    if (rc)
        worst = rc;
    rc = dsp_init();
    rc_line("DSP download (24 steps)", rc);
    if (rc && !worst)
        worst = rc;
    rc = codec_init();
    rc_line("codec AD193x", rc);
    if (rc && !worst)
        worst = rc;
    log_puts("  codec PLL lock after ");
    log_dec(codec_lock_ms());
    log_puts(" ms\n");
    return worst;
}
