/* HID consumer-control interface (UDI) on the ASF UDC: the speaker's keys/IR as media keys for the host, like stock's
 * IF2 (docs/hardware/zd1-main-board.md, "USB"): play/pause, next, previous (the volume keys step the speaker's own volume,
 * and USB audio's status interrupt tells the host). One 1-byte input report, one bit per usage; a press sends the bit, then all-released. */
#ifndef UDI_HID_CC_H
#define UDI_HID_CC_H
#include "conf_usb.h"
#include "usb_protocol.h"
#include "usb_protocol_hid.h"
#include "udd.h"
#include "udc_desc.h"
#include "udi.h"

/* report bits (order = usages in the report descriptor) */
#define HID_CC_PLAYPAUSE 0x01
#define HID_CC_NEXT      0x02
#define HID_CC_PREV      0x04

typedef struct {
    usb_iface_desc_t iface;
    usb_hid_descriptor_t hid;
    usb_ep_desc_t ep;
} udi_hid_cc_desc_t;

#define UDI_HID_CC_REPORT_DESC_SIZE 27
#define UDI_HID_CC_DESC                                                                                                \
    {                                                                                                                  \
        .iface = {sizeof(usb_iface_desc_t), USB_DT_INTERFACE, UDI_HID_CC_IFACE_NUMBER, 0, 1, HID_CLASS,                \
                  HID_SUB_CLASS_NOBOOT, HID_PROTOCOL_GENERIC, 0},                                                      \
        .hid = {sizeof(usb_hid_descriptor_t), USB_DT_HID, LE16(USB_HID_BDC_V1_11), USB_HID_NO_COUNTRY_CODE,            \
                USB_HID_NUM_DESC, USB_DT_HID_REPORT, LE16(UDI_HID_CC_REPORT_DESC_SIZE)},                               \
        .ep = {sizeof(usb_ep_desc_t), USB_DT_ENDPOINT, UDI_HID_CC_EP_IN, USB_EP_TYPE_INTERRUPT, LE16(8), 10},          \
    }

extern UDC_DESC_STORAGE udi_api_t udi_api_hid_cc;

bool hid_cc_ready(void);         /* host configured the interface */
bool hid_cc_press(uint8_t bits); /* press + release; false if not configured or a report is still pending */

#endif
