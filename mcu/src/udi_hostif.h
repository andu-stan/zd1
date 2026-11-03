/* The host link's USB interface (UDI) on the ASF UDC: a vendor interface, its requests on EP0 and the pipe's data on
 * bulk OUT EP 0x05 (hostif.c, docs/protocol/host.md). */
#ifndef UDI_HOSTIF_H
#define UDI_HOSTIF_H
#include "conf_usb.h"
#include "usb_protocol.h"
#include "udd.h"
#include "udc_desc.h"
#include "udi.h"
#include "zd1host_gen.h"

#define UDI_HOSTIF_EP_OUT  (ZD1H_USB_BULK_OUT | USB_EP_DIR_OUT)
#define UDI_HOSTIF_EP_SIZE 64

extern UDC_DESC_STORAGE udi_api_t udi_api_hostif;
typedef struct {
    usb_iface_desc_t iface;
    usb_ep_desc_t ep;
} udi_hostif_desc_t;
#define UDI_HOSTIF_DESC                                                                                                \
    {.iface = {.bLength = sizeof(usb_iface_desc_t),                                                                    \
               .bDescriptorType = USB_DT_INTERFACE,                                                                    \
               .bInterfaceNumber = ZD1H_USB_IFACE,                                                                     \
               .bAlternateSetting = 0,                                                                                 \
               .bNumEndpoints = 1,                                                                                     \
               .bInterfaceClass = ZD1H_USB_CLASS,                                                                      \
               .bInterfaceSubClass = ZD1H_USB_SUBCLASS,                                                                \
               .bInterfaceProtocol = ZD1H_USB_PROTOCOL,                                                                \
               .iInterface = 0},                                                                                       \
     .ep = {.bLength = sizeof(usb_ep_desc_t),                                                                          \
            .bDescriptorType = USB_DT_ENDPOINT,                                                                        \
            .bEndpointAddress = UDI_HOSTIF_EP_OUT,                                                                     \
            .bmAttributes = USB_EP_TYPE_BULK,                                                                          \
            .wMaxPacketSize = LE16(UDI_HOSTIF_EP_SIZE),                                                                \
            .bInterval = 0}}

#endif
