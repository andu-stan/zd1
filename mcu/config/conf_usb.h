#ifndef _CONF_USB_H_
#define _CONF_USB_H_
#include "compiler.h"
#include "board.h"
/* Composite USB device (ASF USB device stack): the speaker as a USB sound card with media keys, and the host link.
 *   IF0 audio control (UAC1: USB-streaming IT -> feature unit mute/volume -> speaker OT), status interrupt IN EP 0x82
 *                                                                                           udi_audio.c
 *   IF1 audio streaming, PCM 16-bit stereo 48/44.1/32 kHz, iso adaptive OUT EP 0x06        udi_audio.c
 *   IF2 HID consumer control (play/pause, next, previous) on EP 0x81                        udi_hid_cc.c
 *   IF3 vendor (FF/5A/01): the host link, control transfers on EP0 (the module's console among them), and bulk OUT
 *       EP 0x05 (the pipe)                                                                  hostif.c
 * Stock's first three interfaces (docs/hardware/zd1-main-board.md, "USB") with ours after them; the CDC port that
 * carried the module's console (IF3+4, EP2-EP4) is gone: the host link carries it. The USBB has 7 endpoints (EP0-6)
 * and under 1 KB of endpoint RAM: audio streaming takes 512 (iso 256 x 2), the pipe 128 (64 x 2 banks), the status
 * interrupt and HID 8 each; EP3 and EP4 are free. The pipe is EP5 and audio EP6 (stock: audio on EP5): ASF frees and
 * re-allocates every endpoint above one whose alternate setting changes, and macOS switches audio's while it plays:
 * with the pipe above audio, a switch mid-transfer loses a 512-byte buffer [HW 2026-10-26]. VID/PID stay the
 * dev-bridge ones (the host tools find the device by them, and the host link's interface by its class); bcdDevice
 * 2.02 marks this descriptor set (2.01: with the CDC port). */

#define USB_DEVICE_VENDOR_ID     0x03EB /* Atmel */
#define USB_DEVICE_PRODUCT_ID    0x2FE0 /* dev-bridge (zd1 finds the device by it) */
#define USB_DEVICE_MAJOR_VERSION 2
#define USB_DEVICE_MINOR_VERSION 2
#define USB_DEVICE_POWER         100 /* mA on VBUS */
#define USB_DEVICE_ATTR          (USB_CONFIG_ATTR_SELF_POWERED)

#define USB_DEVICE_MANUFACTURE_NAME "Bowers & Wilkins" /* stock's strings (19B5:0005: "Bowers & Wilkins", */
#define USB_DEVICE_PRODUCT_NAME     "Zeppelin Air"     /* "Zeppelin Air", "304"): what the host calls it */
#define USB_DEVICE_SERIAL_NAME      "ZD1"              /* fixed, so the host keeps its settings for us */

/* UDC events */
#define UDC_VBUS_EVENT(b_vbus_high)
#define UDC_SUSPEND_EVENT() main_suspend_action()
#define UDC_RESUME_EVENT()  main_resume_action()

/* ---- endpoints / interfaces ---- */
#define USB_DEVICE_EP_CTRL_SIZE 64
#define USB_DEVICE_NB_INTERFACE 4
#define USB_DEVICE_MAX_EP       6

#define UDI_AUDIO_AC_IFACE_NUMBER 0
#define UDI_AUDIO_AS_IFACE_NUMBER 1
#define UDI_AUDIO_EP_OUT          (6 | USB_EP_DIR_OUT) /* 256 B FIFO as stock (stock: EP5; above the pipe) */
#define UDI_AUDIO_EP_SIZE         256
#define UDI_AUDIO_EP_STATUS       (2 | USB_EP_DIR_IN) /* the status interrupt: a volume changed on the speaker */

#define UDI_HID_CC_IFACE_NUMBER 2
#define UDI_HID_CC_EP_IN        (1 | USB_EP_DIR_IN)

/* ---- composite descriptor (ASF udi_composite_desc.c) ---- */
#define UDI_COMPOSITE_DESC_T                                                                                           \
    udi_audio_desc_t udi_audio;                                                                                        \
    udi_hid_cc_desc_t udi_hid_cc;                                                                                      \
    udi_hostif_desc_t udi_hostif

#define UDI_COMPOSITE_DESC_FS .udi_audio = UDI_AUDIO_DESC, .udi_hid_cc = UDI_HID_CC_DESC, .udi_hostif = UDI_HOSTIF_DESC

#define UDI_COMPOSITE_API &udi_api_audio_ac, &udi_api_audio_as, &udi_api_hid_cc, &udi_api_hostif

/* classes/headers last, per ASF convention */
#include "udi_audio.h"
#include "udi_hid_cc.h"
#include "udi_hostif.h"
#include "main.h"
#endif /* _CONF_USB_H_ */
