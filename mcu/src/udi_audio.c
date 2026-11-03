#include "conf_usb.h"
#include "usb_protocol.h"
#include "udd.h"
#include "udc.h"
#include "udi_audio.h"
#include "usbaudio.h"

/* UAC1 class-specific request codes and control selectors */
#define UAC_SET_CUR          0x01
#define UAC_GET_CUR          0x81
#define UAC_GET_MIN          0x82
#define UAC_GET_MAX          0x83
#define UAC_GET_RES          0x84
#define UAC_FU_MUTE          0x01
#define UAC_FU_VOLUME        0x02
#define UAC_EP_SAMPLING_FREQ 0x01

uac_host_t g_uac = {.rate = 48000, .volume = 0};

/* What the host asks of the audio interface, for the log (udi_audio_event): which of it a person's choice of the
 * output makes (docs: mcu/README.md, Intents). Filled here in the interrupt, drained by the main loop. */
static volatile uac_event_t ev[32];
static volatile uint8_t ev_head, ev_tail, ev_lost;
void udi_audio_note(uint8_t what, int32_t value) {
    if ((uint8_t)(ev_head - ev_tail) >= sizeof ev / sizeof ev[0]) {
        ev_lost++;
        return;
    }
    ev[ev_head % 32].what = what;
    ev[ev_head % 32].value = value;
    ev_head++;
}
bool udi_audio_event(uac_event_t *e, uint8_t *lost) {
    if (ev_tail == ev_head)
        return false;
    e->what = ev[ev_tail % 32].what;
    e->value = ev[ev_tail % 32].value;
    ev_tail++;
    *lost = ev_lost;
    ev_lost = 0;
    return true;
}

/* The status interrupt (UDI_AUDIO_EP_STATUS): a volume changed on the speaker is a status word from the feature unit,
 * interrupt pending, the originator the AC interface (UAC1 4.4.2.1); the host reads the volume back (GET_CUR). One in
 * flight; a change while it is pending sends one more after it. */
#ifdef UDI_AUDIO_EP_STATUS
COMPILER_WORD_ALIGNED static uint8_t status_word[2];
static volatile bool st_on, st_busy, st_due;
static void st_send(void);
static void st_sent(udd_ep_status_t status, iram_size_t n, udd_ep_id_t ep) {
    (void)n;
    (void)ep;
    st_busy = false;
    if (status != UDD_EP_TRANSFER_OK)
        return; /* aborted: the interface went down */
    udi_audio_note(UAC_EV_STATUS, 1);
    if (st_due && st_on)
        st_send();
}
static void st_send(void) {
    st_due = false;
    status_word[0] = 0x80; /* interrupt pending; originator 0: the AC interface */
    status_word[1] = UAC_UNIT_FU;
    st_busy = udd_ep_run(UDI_AUDIO_EP_STATUS, false, status_word, sizeof status_word, st_sent);
    if (st_busy)
        udi_audio_note(UAC_EV_STATUS, 0);
}
static void status_notify(void) {
    irqflags_t f = cpu_irq_save();
    if (st_on && st_busy)
        st_due = true;
    else if (st_on)
        st_send();
    cpu_irq_restore(f);
}
static bool ac_enable(void) {
    st_busy = st_due = false;
    st_on = true;
    return true;
}
static void ac_disable(void) { st_on = false; }
#else
static void status_notify(void) {}
static bool ac_enable(void) { return true; }
static void ac_disable(void) {}
#endif
static bool ac_setup(void);
static uint8_t ac_getsetting(void) { return 0; }
static bool as_enable(void);
static void as_disable(void);
static bool as_setup(void);
static uint8_t as_getsetting(void);

UDC_DESC_STORAGE udi_api_t udi_api_audio_ac = {
    .enable = ac_enable, .disable = ac_disable, .setup = ac_setup, .getsetting = ac_getsetting, .sof_notify = NULL};
UDC_DESC_STORAGE udi_api_t udi_api_audio_as = {
    .enable = as_enable, .disable = as_disable, .setup = as_setup, .getsetting = as_getsetting, .sof_notify = NULL};

COMPILER_WORD_ALIGNED static uint8_t ctrl_buf[4]; /* control data stage, both directions */

static uint16_t rd_le16(const uint8_t *b) { return (uint16_t)(b[0] | b[1] << 8); }
static void wr_le16(uint8_t *b, uint16_t v) {
    b[0] = (uint8_t)v;
    b[1] = (uint8_t)(v >> 8);
}
static bool reply(uint16_t n) {
    udd_g_ctrlreq.payload = ctrl_buf;
    udd_g_ctrlreq.payload_size = min(n, udd_g_ctrlreq.req.wLength);
    return true;
}

void udi_audio_set_volume(int16_t v) {
    v = v < UAC_VOL_MIN ? UAC_VOL_MIN : v > UAC_VOL_MAX ? UAC_VOL_MAX : v;
    if (v == g_uac.volume)
        return;
    g_uac.volume = v;
    status_notify();
}

/* ---- audio control interface: feature unit 2, master channel ---- */
static void fu_set_done(void) {
    uint8_t cs = udd_g_ctrlreq.req.wValue >> 8;
    if (cs == UAC_FU_MUTE) {
        g_uac.mute = ctrl_buf[0] != 0;
        g_uac.changed |= UAC_CHG_MUTE;
        udi_audio_note(UAC_EV_SET_MUTE, g_uac.mute);
    } else if (cs == UAC_FU_VOLUME) {
        int16_t v = (int16_t)rd_le16(ctrl_buf);
        if (v == (int16_t)0x8000)
            v = UAC_VOL_MIN; /* -inf */
        g_uac.volume = v < UAC_VOL_MIN ? UAC_VOL_MIN : v > UAC_VOL_MAX ? UAC_VOL_MAX : v;
        g_uac.changed |= UAC_CHG_VOLUME;
        udi_audio_note(UAC_EV_SET_VOL, v);
    }
}
static bool ac_setup(void) {
    if (Udd_setup_recipient() != USB_REQ_RECIP_INTERFACE || Udd_setup_type() != USB_REQ_TYPE_CLASS)
        return false;
    uint8_t entity = udd_g_ctrlreq.req.wIndex >> 8, cs = udd_g_ctrlreq.req.wValue >> 8,
            cn = udd_g_ctrlreq.req.wValue & 0xFF;
    if (entity != UAC_UNIT_FU || cn != 0 || (cs != UAC_FU_MUTE && cs != UAC_FU_VOLUME)) {
        udi_audio_note(UAC_EV_REFUSED, (int32_t)((uint32_t)udd_g_ctrlreq.req.bRequest << 24 | (uint32_t)entity << 16 |
                                                 udd_g_ctrlreq.req.wValue));
        return false;
    }
    uint16_t len = cs == UAC_FU_MUTE ? 1 : 2;
    if (Udd_setup_is_in()) {
        int16_t v;
        udi_audio_note(UAC_EV_GET, udd_g_ctrlreq.req.bRequest << 8 | cs);
        switch (udd_g_ctrlreq.req.bRequest) {
        case UAC_GET_CUR:
            v = cs == UAC_FU_MUTE ? g_uac.mute : g_uac.volume;
            break;
        case UAC_GET_MIN:
            v = UAC_VOL_MIN;
            break;
        case UAC_GET_MAX:
            v = UAC_VOL_MAX;
            break;
        case UAC_GET_RES:
            v = UAC_VOL_RES;
            break;
        default:
            return false;
        }
        if (cs == UAC_FU_MUTE && udd_g_ctrlreq.req.bRequest != UAC_GET_CUR)
            return false;
        if (len == 1)
            ctrl_buf[0] = (uint8_t)v;
        else
            wr_le16(ctrl_buf, (uint16_t)v);
        return reply(len);
    }
    if (udd_g_ctrlreq.req.bRequest != UAC_SET_CUR || udd_g_ctrlreq.req.wLength != len)
        return false;
    udd_g_ctrlreq.payload = ctrl_buf;
    udd_g_ctrlreq.payload_size = len;
    udd_g_ctrlreq.callback = fu_set_done;
    return true;
}

/* ---- audio streaming interface ---- */
static uint8_t as_alt;
static volatile uint32_t packets;
COMPILER_WORD_ALIGNED static uint8_t pkt[2][UDI_AUDIO_EP_SIZE];
static uint8_t pkt_i;

uint32_t udi_audio_packets(void) { return packets; }

static void rx_done(udd_ep_status_t status, iram_size_t n, udd_ep_id_t ep);
static void rx_arm(void) {
    pkt_i ^= 1;
    udd_ep_run(UDI_AUDIO_EP_OUT, false, pkt[pkt_i], UDI_AUDIO_EP_SIZE, rx_done);
}
/* One iso packet landed (USB interrupt): re-arm into the other buffer first, then hand this one over. */
static void rx_done(udd_ep_status_t status, iram_size_t n, udd_ep_id_t ep) {
    (void)ep;
    if (status != UDD_EP_TRANSFER_OK || as_alt != 1)
        return; /* aborted: alt 0, reset or detach */
    uint8_t *done = pkt[pkt_i];
    rx_arm();
    packets++;
    usbaudio_rx(done, n);
}

static bool as_enable(void) {
    as_alt = udc_get_interface_desc()->bAlternateSetting;
    if (as_alt == 1)
        rx_arm();
    g_uac.streaming = as_alt == 1;
    g_uac.changed |= UAC_CHG_ALT;
    udi_audio_note(UAC_EV_ALT, as_alt);
    return true;
}
static void as_disable(void) {
    as_alt = 0;
    g_uac.streaming = false;
    g_uac.changed |= UAC_CHG_ALT;
    udi_audio_note(UAC_EV_ALT, -1);
}
static uint8_t as_getsetting(void) { return as_alt; }

static void freq_set_done(void) {
    g_uac.rate = ctrl_buf[0] | (uint32_t)ctrl_buf[1] << 8 | (uint32_t)ctrl_buf[2] << 16;
    g_uac.changed |= UAC_CHG_RATE;
    udi_audio_note(UAC_EV_RATE, (int32_t)g_uac.rate);
}
static bool as_setup(void) {
    if (Udd_setup_recipient() != USB_REQ_RECIP_ENDPOINT || Udd_setup_type() != USB_REQ_TYPE_CLASS)
        return false;
    if ((udd_g_ctrlreq.req.wIndex & 0xFF) != UDI_AUDIO_EP_OUT ||
        (udd_g_ctrlreq.req.wValue >> 8) != UAC_EP_SAMPLING_FREQ)
        return false;
    if (Udd_setup_is_in()) {
        if (udd_g_ctrlreq.req.bRequest != UAC_GET_CUR)
            return false;
        uint32_t r = g_uac.rate;
        ctrl_buf[0] = (uint8_t)r;
        ctrl_buf[1] = (uint8_t)(r >> 8);
        ctrl_buf[2] = (uint8_t)(r >> 16);
        return reply(3);
    }
    if (udd_g_ctrlreq.req.bRequest != UAC_SET_CUR || udd_g_ctrlreq.req.wLength != 3)
        return false;
    udd_g_ctrlreq.payload = ctrl_buf;
    udd_g_ctrlreq.payload_size = 3;
    udd_g_ctrlreq.callback = freq_set_done;
    return true;
}
