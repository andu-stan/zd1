/* Rear RGB LED on PWM1/2/3 (PB20/PB21/PB22), period 400, stock CMR 0x20A. The source manager says what the speaker
 * is doing (a look: state, source, the module's receiver, what it does); this draws it: a colour per source and per
 * receiver, crossfades between looks, breathing while ready / paused, steady while playing, dim while the amp
 * sleeps, the volume as brightness (mcu/README.md, The LED). B&W's own patterns: re/mcu/extracted/led_patterns.txt,
 * docs/hardware/zd1-main-board.md. */
#ifndef LED_H
#define LED_H
#include <stdint.h>
#include <stdbool.h>

typedef enum { LOOK_BOOT, LOOK_STANDBY, LOOK_ERROR, LOOK_RUN } led_kind_t;
typedef enum {
    ACT_PLAYING,  /* the source has sound */
    ACT_READY,    /* selected, silent (a paused sender, no stream) */
    ACT_ASLEEP,   /* silent long enough that the amp went off */
    ACT_NO_NET,   /* Wi-Fi: the module has no network */
    ACT_STARTING, /* Wi-Fi: the module isn't up yet */
    ACT_CHOOSING, /* ON/OFF presses: the candidate source, not switched to yet */
} led_act_t;
typedef struct {
    uint8_t kind, source, player, act;
} led_look_t; /* source_t, ZD1_PLAYER_*, led_act_t */

typedef enum { FLASH_PURPLE, FLASH_GREEN, FLASH_RED } led_flash_t;

void led_init(void);
void led_show(const led_look_t *l);           /* what the speaker does now (cheap when unchanged: call it often) */
void led_volume(uint8_t step, bool limit);    /* a local volume change: feedback for a second */
void led_ack(uint8_t source, uint8_t player); /* a person picked this source: two bright pulses in its colour */
void led_flash(led_flash_t c, uint32_t ms);   /* a fast blink over everything for ms (the gate's prompts) */
void led_poll(uint32_t now);
uint8_t led_code(void); /* the look in a byte (Diag): source | act << 3 | kind << 6 */

#endif
