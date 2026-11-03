#include "conf_usb.h"
#include "usb_protocol.h"
#include "udd.h"
#include "udc.h"
#include "udi_hid.h"
#include "udi_hid_cc.h"

static bool cc_enable(void);
static void cc_disable(void);
static bool cc_setup(void);
static uint8_t cc_getsetting(void) { return 0; }

UDC_DESC_STORAGE udi_api_t udi_api_hid_cc = {
    .enable = cc_enable, .disable = cc_disable, .setup = cc_setup, .getsetting = cc_getsetting, .sof_notify = NULL};

COMPILER_WORD_ALIGNED static uint8_t cc_rate, cc_protocol;
COMPILER_WORD_ALIGNED static uint8_t report; /* in flight */
static volatile bool enabled, busy, release_due;

COMPILER_WORD_ALIGNED
UDC_DESC_STORAGE uint8_t udi_hid_cc_report_desc[UDI_HID_CC_REPORT_DESC_SIZE] = {
    0x05, 0x0C, /* Usage Page (Consumer) */
    0x09, 0x01, /* Usage (Consumer Control) */
    0xA1, 0x01, /* Collection (Application) */
    0x15, 0x00, /*   Logical Minimum (0) */
    0x25, 0x01, /*   Logical Maximum (1) */
    0x75, 0x01, /*   Report Size (1) */
    0x95, 0x03, /*   Report Count (3) */
    0x09, 0xCD, /*   Play/Pause */
    0x09, 0xB5, /*   Scan Next Track */
    0x09, 0xB6, /*   Scan Previous Track */
    0x81, 0x02, /*   Input (Data, Variable, Absolute) */
    0x95, 0x05, /*   Report Count (5) */
    0x81, 0x03, /*   Input (Constant): padding */
    0xC0        /* End Collection */
};

static bool cc_enable(void) {
    cc_rate = 0;
    cc_protocol = 0;
    busy = release_due = false;
    enabled = true;
    return true;
}
static void cc_disable(void) { enabled = false; }
static bool no_set_report(void) { return false; }
static bool cc_setup(void) { return udi_hid_setup(&cc_rate, &cc_protocol, udi_hid_cc_report_desc, no_set_report); }

static bool send(uint8_t bits);
static void sent(udd_ep_status_t status, iram_size_t n, udd_ep_id_t ep) {
    (void)status;
    (void)n;
    (void)ep;
    busy = false;
    if (release_due) {
        release_due = false;
        send(0);
    }
}
static bool send(uint8_t bits) {
    report = bits;
    busy = udd_ep_run(UDI_HID_CC_EP_IN, false, &report, 1, sent);
    return busy;
}

bool hid_cc_ready(void) { return enabled; }
bool hid_cc_press(uint8_t bits) {
    irqflags_t f = cpu_irq_save();
    bool ok = enabled && !busy && !release_due && send(bits);
    if (ok)
        release_due = true;
    cpu_irq_restore(f);
    return ok;
}
