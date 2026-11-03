/* USB audio streaming engine: iso OUT packets (udi_audio.c) -> ring buffer -> PDCA ch0 -> SSC (I2S, bit clock from
 * RX_CLK PA18 like stock's ssc_i2s mode STEREO_OUT_EXT_CLK) -> DSP input 1.
 * The endpoint is adaptive, so the speaker follows the host: the ring fill level is held near its target by trimming
 * the CS2000 ratio (= MCLK = the whole DSP/codec/I2S clock) a few ppm at a time. Underrun -> silence blocks, never
 * stale data; the stream re-primes before playing again. Rate/alt changes from the host are applied here in the main
 * loop (I2C, SSC set-up), never in the USB interrupt. */
#ifndef USBAUDIO_H
#define USBAUDIO_H
#include <compiler.h>

void usbaudio_rx(const uint8_t *pkt, uint32_t bytes); /* USB interrupt: one iso packet, PCM16 LE stereo */
void usbaudio_poll(uint32_t now);                     /* main loop: host changes, rate trim */
/* Sound from the host, whichever the source (the samples are looked at even while they are dropped): a session starts
 * with the first loud packet and ends after SOUND_END_MS of silence or when the host closes the stream, so a gap
 * between tracks doesn't make a new one. */
bool usbaudio_session(uint32_t *since);      /* a session runs; since = when it started */
bool usbaudio_sounding(void);                /* sound in the last 500 ms */
uint32_t usbaudio_sounding_ms(uint32_t now); /* how long it has sounded without a gap over 500 ms (0: silent) */
bool usbaudio_take_pick(void);               /* the computer picked this speaker as its output (once) */
bool usbaudio_take_open(void);               /* the host opened the stream (once): sound is likely coming */
uint8_t usbaudio_volmax(void);               /* the host-volume cap, also used for AirPlay */
void usbaudio_local_volume(uint8_t step);    /* the speaker's volume changed: tell the host (udi_audio.c) */
typedef struct {
    bool running, clock_ok, primed;
    uint16_t fill;
    uint32_t under, over, dropped, play_fps, host_fps, ratio, nominal;
    uint8_t host_step;
} usbaudio_stats_t;
void usbaudio_stats(usbaudio_stats_t *s); /* for the host link's Diag */

#endif
