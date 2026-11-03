/* The module as the speaker's Wi-Fi source (wifi.c): one picture of it, filled by our OS over the native registers
 * (reg 0xD5 = this firmware's protocol, dm870_native()). Fed from dm870.c's notification drain; driven from the main
 * loop. */
#ifndef WIFI_H
#define WIFI_H
#include <compiler.h>

#include "zd1link.h" /* the SPI protocol: its values (generated from protocol/spi) and the code on them */

typedef enum { WIFI_PLAY_UNKNOWN = 0, WIFI_PLAY_EMPTY, WIFI_PLAY_PLAYING, WIFI_PLAY_PAUSED } wifi_play_t;

void wifi_notify(uint8_t reg, const uint8_t *b, uint16_t n); /* from dm870.c, every drained notification */
void wifi_module_ready(void);                                /* the module (re)greeted: the speaker's snapshot (0xD2) */
/* the Wi-Fi source was selected: the snapshot says so; resume: its receiver in front is chosen (0xC2) */
void wifi_enter(bool resume);
void wifi_leave(void);        /* leaving Wi-Fi (other source, standby, error): the snapshot says so */
void wifi_key(uint8_t key);   /* a transport key while on Wi-Fi: ZD1_NATIVE_KEY_* on 0xD3 */
void wifi_local_volume(void); /* every volume change on the speaker: the snapshot */
bool wifi_net_connected(void);
wifi_play_t wifi_play_state(void);
uint8_t wifi_player(uint8_t *state); /* our OS's receiver in front (ZD1_PLAYER_*) and its state; NONE otherwise */
uint8_t wifi_sessions(void);         /* our OS's receivers with a sender connected: bit (ZD1_PLAYER_*) */
const char *wifi_player_name(uint8_t player); /* ZD1_PLAYER_*, for the log */
void wifi_select(uint8_t player); /* a receiver chosen on the speaker: it resumes, the others pause (0xC2) */
int wifi_pending_volume(void);    /* a volume the module asked for that waits for it to be the input, -1 = none */
#endif
