/* Audio platform bring-up (CS2000, DSP, codec, as stock does: docs/mcu/stock-firmware.md "Start-up"; the amp stays
 * off) and the report it prints. Source selection/policy lives in the source manager. */
#ifndef AUDIO_H
#define AUDIO_H
#include <compiler.h>

int audio_init(void); /* 0 = OK; prints a per-stage report */
#endif
