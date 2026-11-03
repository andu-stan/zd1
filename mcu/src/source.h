/* Source manager + system state machine: STANDBY / RUN(source) / ERROR, source detection and selection, amp
 * sequencing (supply, 2 s soft start, unmute, volume ramp), user volume/bass, what the LED shows. Non-blocking.
 * Ours on top of stock's: the amp goes off after a source has been silent for 5 minutes and comes back with sound;
 * standby holds the amp supply 30 s; a USB stream that starts takes the speaker (newest wins, never right after a
 * choice made by hand); volume keys on USB go to the host as HID keys; the volume stays under the heat cap. */
#ifndef SOURCE_H
#define SOURCE_H
#include <stdint.h>
#include <stdbool.h>

typedef enum { SRC_NONE = 0, SRC_USB, SRC_AUX, SRC_WIFI, SRC_IPOD, SRC_N } source_t;
typedef enum { SYS_BOOT = 0, SYS_STANDBY, SYS_RUN, SYS_ERROR } sys_state_t;

void source_init(void); /* after audio_init + settings_load */
void source_poll(uint32_t now);
/* by hand: select + wake; false: not there (unplugged, the iPod), or a protection tripped */
bool source_select(source_t s);
void source_auto_select(source_t s);            /* by itself (a stream started): select + wake */
void source_intent(source_t s, uint8_t player); /* a person picked it (a plug, a phone, an app): switch now, LED, amp */
/* by hand: Wi-Fi and that receiver of our OS; false: no sender connected, or a protection tripped */
bool source_select_player(uint8_t player);
void source_standby(void);
sys_state_t source_state(void);
source_t source_current(void);
void source_error(uint8_t code);       /* protections: amp off, mute, error LED, stays until cleared */
void source_volume_set(uint8_t step);  /* absolute user step 0..VOL_MAX from AirPlay / the host link (no LED flash) */
void source_host_volume(uint8_t step); /* the USB host's volume */
void source_ramp_in(void);             /* USB's engine (re)starts: mute now, ramp back up in 200 ms */
void source_host_mute(bool mute);      /* USB feature-unit mute, effective while USB is the source */
bool source_amp_allowed(void);         /* the amp interlock (allowed from boot unless the EEPROM says locked) */
void source_amp_allow(bool allow, bool keep); /* deny switches the amp off at once; keep: also from every boot */
bool source_amp_asleep(void);                 /* RUN, the amp off because the source has been silent */
uint32_t source_silent_ms(uint32_t now);      /* RUN: how long the source has been silent (0: it plays) */
int32_t source_level(void); /* the DSP's input meter, last sample, while it is read (AUX, the stock app's Wi-Fi) */
uint32_t source_level_floor(void);  /* ... and the source's learnt noise floor (sound: 12 dB over it, -80..-40 dBFS) */
void source_bass_set(uint8_t bass); /* 0..BASS_MAX, kept in the EEPROM */
void source_key(uint8_t key);       /* key_event_t (inputs.h): as if pressed (the host link's Key) */
uint8_t source_error_code(void);    /* ERROR: which protection tripped */
#endif
