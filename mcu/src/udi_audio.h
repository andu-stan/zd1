/* USB Audio Class 1.0 device interface (UDI) for the ASF UDC: ASF has no audio class, this is ours.
 * One playback path, the same shape as stock (docs/hardware/zd1-main-board.md, "USB"):
 *   IT 1 (USB streaming, stereo) -> FU 2 (master mute + volume) -> OT 3 (speaker)
 *   AS interface: alt 0 = idle, alt 1 = PCM 16-bit stereo, 48 / 44.1 / 32 kHz, iso adaptive OUT, sampling-frequency
 *   control on the endpoint.
 *   With UDI_AUDIO_EP_STATUS (conf_usb.h): the AC interface's status interrupt (UAC1 3.7.1.2, 4.4.2.1), a 2-byte
 *   status word from the feature unit when the volume changes on the speaker; the host answers with GET_CUR (macOS
 *   [HW 2026-11-02]).
 * Everything here runs in the USB interrupt: it only records what the host asked for (rate, volume, mute, alt
 * setting) and hands received packets to usbaudio.c; the main loop acts on it (I2C, SSC/PDCA set-up). */
#ifndef UDI_AUDIO_H
#define UDI_AUDIO_H
#include "conf_usb.h"
#include "usb_protocol.h"
#include "udd.h"
#include "udc_desc.h"
#include "udi.h"

/* feature-unit volume range, 1/256 dB: the DSP's own range, 1 dB per user step (step = dB + 61, 71 = +10 dB) */
#define UAC_VOL_MIN ((int16_t)(-60 * 256))
#define UAC_VOL_MAX ((int16_t)(10 * 256))
#define UAC_VOL_RES ((int16_t)(1 * 256))

// cppcheck-suppress unknownMacro ; ASF (compiler.h)
COMPILER_PACK_SET(1)
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype;
    le16_t bcdADC, wTotalLength;
    uint8_t bInCollection, baInterfaceNr1;
} uac_ac_header_t;
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype, bTerminalID;
    le16_t wTerminalType;
    uint8_t bAssocTerminal, bNrChannels;
    le16_t wChannelConfig;
    uint8_t iChannelNames, iTerminal;
} uac_it_t;
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype, bUnitID, bSourceID, bControlSize;
    uint8_t bmaControls[3];
    uint8_t iFeature;
} uac_fu_t;
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype, bTerminalID;
    le16_t wTerminalType;
    uint8_t bAssocTerminal, bSourceID, iTerminal;
} uac_ot_t;
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype, bTerminalLink, bDelay;
    le16_t wFormatTag;
} uac_as_general_t;
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype, bFormatType, bNrChannels, bSubframeSize, bBitResolution,
        bSamFreqType;
    uint8_t tSamFreq[3][3];
} uac_format_t;
typedef struct {
    uint8_t bLength, bDescriptorType, bEndpointAddress, bmAttributes;
    le16_t wMaxPacketSize;
    uint8_t bInterval, bRefresh, bSynchAddress;
} uac_ep_t; /* 9-byte audio endpoint */
typedef struct {
    uint8_t bLength, bDescriptorType, bDescriptorSubtype, bmAttributes, bLockDelayUnits;
    le16_t wLockDelay;
} uac_cs_ep_t;

typedef struct {
    usb_iface_desc_t ac;
    uac_ac_header_t ac_header;
    uac_it_t it;
    uac_fu_t fu;
    uac_ot_t ot;
#ifdef UDI_AUDIO_EP_STATUS
    uac_ep_t status_ep; /* standard AC interrupt endpoint: 9 bytes, bRefresh and bSynchAddress 0 */
#endif
    usb_iface_desc_t as_alt0;
    usb_iface_desc_t as_alt1;
    uac_as_general_t as_general;
    uac_format_t format;
    uac_ep_t ep;
    uac_cs_ep_t cs_ep;
} udi_audio_desc_t;
COMPILER_PACK_RESET()

#define UAC_TERM_IT  1
#define UAC_UNIT_FU  2
#define UAC_TERM_OT  3
#define UAC_AC_TOTAL (sizeof(uac_ac_header_t) + sizeof(uac_it_t) + sizeof(uac_fu_t) + sizeof(uac_ot_t))
#define UAC_FREQ(f)  {(f) & 0xFF, ((f) >> 8) & 0xFF, ((f) >> 16) & 0xFF}

#ifdef UDI_AUDIO_EP_STATUS
#define UAC_AC_NB_EP 1
#define UAC_STATUS_EP_DESC                                                                                             \
    , .status_ep = {sizeof(uac_ep_t), USB_DT_ENDPOINT, UDI_AUDIO_EP_STATUS, USB_EP_TYPE_INTERRUPT, LE16(2), 10, 0, 0}
#else
#define UAC_AC_NB_EP 0
#define UAC_STATUS_EP_DESC
#endif

#define UDI_AUDIO_DESC                                                                                                 \
    {.ac = {sizeof(usb_iface_desc_t), USB_DT_INTERFACE, UDI_AUDIO_AC_IFACE_NUMBER, 0, UAC_AC_NB_EP, 0x01, 0x01, 0, 0}, \
     .ac_header = {sizeof(uac_ac_header_t), 0x24, 0x01, LE16(0x0100), LE16(UAC_AC_TOTAL), 1,                           \
                   UDI_AUDIO_AS_IFACE_NUMBER},                                                                         \
     .it = {sizeof(uac_it_t), 0x24, 0x02, UAC_TERM_IT, LE16(0x0101), 0, 2, LE16(0x0003), 0, 0},                        \
     .fu = {sizeof(uac_fu_t), 0x24, 0x06, UAC_UNIT_FU, UAC_TERM_IT, 1, {0x03, 0x00, 0x00}, 0},                         \
     .ot = {sizeof(uac_ot_t), 0x24, 0x03, UAC_TERM_OT, LE16(0x0301), 0, UAC_UNIT_FU, 0},                               \
     .as_alt0 = {sizeof(usb_iface_desc_t), USB_DT_INTERFACE, UDI_AUDIO_AS_IFACE_NUMBER, 0, 0, 0x01, 0x02, 0x00, 0},    \
     .as_alt1 = {sizeof(usb_iface_desc_t), USB_DT_INTERFACE, UDI_AUDIO_AS_IFACE_NUMBER, 1, 1, 0x01, 0x02, 0x00, 0},    \
     .as_general = {sizeof(uac_as_general_t), 0x24, 0x01, UAC_TERM_IT, 1, LE16(0x0001)},                               \
     .format =                                                                                                         \
         {sizeof(uac_format_t), 0x24, 0x02, 1, 2, 2, 16, 3, {UAC_FREQ(48000), UAC_FREQ(44100), UAC_FREQ(32000)}},      \
     .ep = {sizeof(uac_ep_t), USB_DT_ENDPOINT, UDI_AUDIO_EP_OUT, 0x09 /* iso, adaptive */, LE16(UDI_AUDIO_EP_SIZE), 1, \
            0, 0},                                                                                                     \
     .cs_ep = {sizeof(uac_cs_ep_t), 0x25, 0x01, 0x01 /* sampling frequency control */, 0, LE16(0)} UAC_STATUS_EP_DESC}

extern UDC_DESC_STORAGE udi_api_t udi_api_audio_ac;
extern UDC_DESC_STORAGE udi_api_t udi_api_audio_as;

/* host-side state, written in the USB interrupt (volatile, read by the main loop) */
typedef struct {
    volatile bool streaming;  /* AS alt 1 selected */
    volatile uint32_t rate;   /* last SET_CUR sampling frequency (Hz), default 48000 */
    volatile int16_t volume;  /* FU master volume, 1/256 dB */
    volatile bool mute;       /* FU master mute */
    volatile uint8_t changed; /* UAC_CHG_* bits; the main loop clears them */
} uac_host_t;
#define UAC_CHG_ALT    0x01
#define UAC_CHG_RATE   0x02
#define UAC_CHG_VOLUME 0x04
#define UAC_CHG_MUTE   0x08
extern uac_host_t g_uac;

/* the host's requests, for the log */
enum {
    UAC_EV_ALT,
    UAC_EV_RATE,
    UAC_EV_SET_VOL,
    UAC_EV_SET_MUTE,
    UAC_EV_GET,
    UAC_EV_SUSPEND,
    UAC_EV_RESUME,
    UAC_EV_STATUS,  /* the status word: 0 queued, 1 the host took it */
    UAC_EV_REFUSED, /* a request to the AC interface we stall: bRequest << 24 | entity << 16 | wValue */
};
typedef struct {
    uint8_t what;
    int32_t value;
} uac_event_t;                                       /* ALT: the setting (-1: disabled); GET: req << 8 | cs */
void udi_audio_note(uint8_t what, int32_t value);    /* interrupt (and main_suspend/resume_action) */
bool udi_audio_event(uac_event_t *e, uint8_t *lost); /* main loop: the next one, false when none */

void udi_audio_set_volume(int16_t v); /* a change on the speaker: what GET_CUR reports (and the status word) */
uint32_t udi_audio_packets(void);     /* iso packets received since boot */

#endif
