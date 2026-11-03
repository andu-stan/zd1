/* Persistent user settings in the 24C02 EEPROM (I2C 0x50). Our own block at 0x80..0x8F, two 8-byte pages each with
 * its magic + CRC (a firmware that knows only the first page leaves the second alone); stock's 0x60-byte block at
 * 0x00 is left untouched. Writes are deferred (SETTINGS_SAVE_DELAY_MS after the last change). */
#ifndef SETTINGS_H
#define SETTINGS_H
#include <stdint.h>
#include <stdbool.h>

#define VOL_MAX  71 /* the loudest user step: 0 = mute .. 71 (stock's 72 steps; DSP index = DSP_VOL_MUTE - step) */
#define BASS_MAX 4  /* the last of stock's bass presets */

typedef struct {
    uint8_t vol_step;     /* 0..VOL_MAX */
    uint8_t bass;         /* 0..BASS_MAX */
    uint8_t last_source;  /* source_t */
    uint8_t flags;        /* SETTINGS_* */
    uint8_t amp_idle_min; /* a source silent this long: the amp off (SETTINGS_AMP_IDLE_*; page 2) */
    uint8_t vol_cap;      /* the highest step the USB host and the module may set (page 2) */
} settings_t;

#define SETTINGS_GATE_OPEN      0x01 /* the host link's gate open for good (hostif.c): a unit tools drive unattended */
#define SETTINGS_BOOT_GOOD_B    0x02 /* the module's good system slot is B (else A; modboot.c) */
#define SETTINGS_BOOT_TRY_MASK  0x0C /* a ZD1H_SLOT_* to start once at the module's next boot (0: none) */
#define SETTINGS_BOOT_TRY_SHIFT 2
#define SETTINGS_AMP_LOCKED     0x10 /* the amp locked at every boot (the host link's Amp deny with keep); clear: allowed */
#define SETTINGS_NO_AUTO_SWITCH 0x40 /* a USB stream doesn't take the speaker by itself */

#define SETTINGS_AMP_IDLE_DEFAULT 5 /* min, unless set (the host link's Prefs) */
#define SETTINGS_AMP_IDLE_MIN     1
#define SETTINGS_AMP_IDLE_MAX     30 /* the amp runs hot: never longer */

#define SETTINGS_VOL_CAP_DEFAULT 40 /* the safety cap kept since the first firmware */

#define SETTINGS_SAVE_DELAY_MS 3000
#define SETTINGS_DEFAULT_VOL   30 /* DSP index 41 = -31 dB */

extern settings_t g_settings;
bool settings_load(void);         /* false = blank/corrupt, defaults loaded */
void settings_changed(void);      /* schedule a save */
void settings_poll(uint32_t now); /* does the deferred save */
bool settings_save_now(void);

#endif
