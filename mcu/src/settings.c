#include <string.h>
#include <delay.h>
#include "i2c.h"
#include "log.h"
#include "tick.h"
#include "settings.h"

#define EE_ADDR   0x80 /* page-aligned (24C02 pages are 8 bytes) */
#define EE_MAGIC0 'Z'
#define EE_MAGIC1 'D'
#define EE_VER    1
#define EE_ADDR2  0x88 /* the second page: [Z][E][ver][amp idle min][volume cap][firmware seen, LE 16][crc] */
#define EE_MAGIC2 'E'

#define DEFAULTS {SETTINGS_DEFAULT_VOL, 0, 0, 0, SETTINGS_AMP_IDLE_DEFAULT, SETTINGS_VOL_CAP_DEFAULT, 0}
settings_t g_settings = DEFAULTS;
static const settings_t defaults = DEFAULTS;
static bool dirty;
static uint32_t save_at;

static uint8_t crc8(const uint8_t *b, unsigned n) {
    uint8_t c = 0xFF;
    while (n--) {
        c ^= *b++;
        for (int i = 0; i < 8; i++)
            c = (c & 0x80) ? (uint8_t)(c << 1) ^ 0x31 : (uint8_t)(c << 1);
    }
    return c;
}

/* page 1: [Z][D][ver][vol][bass][src][flags][crc]; page 2 has its own magic and CRC, so a blank page 2 leaves page 1
 * valid and takes the defaults */
bool settings_load(void) {
    uint8_t b[8];
    g_settings = defaults;
    if (!(i2c_read(I2C_EEPROM, EE_ADDR, 1, b, 8) == 0 && b[0] == EE_MAGIC0 && b[1] == EE_MAGIC1 && b[2] == EE_VER &&
          b[7] == crc8(b, 7) && b[3] <= VOL_MAX && b[4] <= BASS_MAX))
        return false;
    g_settings.vol_step = b[3];
    g_settings.bass = b[4];
    g_settings.last_source = b[5];
    g_settings.flags = b[6];
    if (i2c_read(I2C_EEPROM, EE_ADDR2, 1, b, 8) == 0 && b[0] == EE_MAGIC0 && b[1] == EE_MAGIC2 && b[2] == EE_VER &&
        b[7] == crc8(b, 7)) {
        if (b[3] >= SETTINGS_AMP_IDLE_MIN && b[3] <= SETTINGS_AMP_IDLE_MAX)
            g_settings.amp_idle_min = b[3];
        if (b[4] >= 1 && b[4] <= VOL_MAX)
            g_settings.vol_cap = b[4];
        g_settings.fw_seen = (uint16_t)(b[5] | b[6] << 8);
    }
    if (g_settings.vol_step > g_settings.vol_cap)
        g_settings.vol_step = g_settings.vol_cap; /* the one limit, at boot too */
    return true;
}

/* After a page write the 24C02 programs it (its write cycle, up to 5 ms) and answers nothing meanwhile: the next
 * transfer waits for its ACK (a one-byte read), up to 10 ms */
static bool ee_written(void) {
    uint8_t x;
    for (int i = 0; i < 20; i++) {
        delay_us(500);
        if (i2c_read(I2C_EEPROM, EE_ADDR, 1, &x, 1) == 0)
            return true;
    }
    return false;
}

/* one page each (no wrap); a page the EEPROM didn't take leaves the save pending, tried again later */
bool settings_save_now(void) {
    static bool failing;
    uint8_t b[8] = {EE_MAGIC0,        EE_MAGIC1, EE_VER, g_settings.vol_step, g_settings.bass, g_settings.last_source,
                    g_settings.flags, 0};
    b[7] = crc8(b, 7);
    bool ok = i2c_write(I2C_EEPROM, EE_ADDR, 1, b, 8) == 0 && ee_written();
    uint8_t c[8] = {EE_MAGIC0,
                    EE_MAGIC2,
                    EE_VER,
                    g_settings.amp_idle_min,
                    g_settings.vol_cap,
                    (uint8_t)g_settings.fw_seen,
                    (uint8_t)(g_settings.fw_seen >> 8),
                    0};
    c[7] = crc8(c, 7);
    ok = i2c_write(I2C_EEPROM, EE_ADDR2, 1, c, 8) == 0 && ee_written() && ok;
    dirty = !ok;
    if (!ok) {
        save_at = tick_ms() + SETTINGS_SAVE_DELAY_MS;
        if (!failing)
            log_puts("[settings] the EEPROM didn't take them: trying again every 3 s\n");
    } else if (failing)
        log_puts("[settings] saved\n");
    failing = !ok;
    return ok;
}

void settings_changed(void) {
    dirty = true;
    save_at = tick_ms() + SETTINGS_SAVE_DELAY_MS;
}
void settings_poll(uint32_t now) {
    if (dirty && tick_after(now, save_at))
        settings_save_now();
}
