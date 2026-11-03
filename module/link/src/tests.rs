//! The protocol against a fake SPI slave: what the MCU clocks in, what it gets back.
extern crate std;
use super::NOTE_MAX;
use super::*;
use dm870::gpio::{self, Pin};
use dm870::spi_slave::BASE as SPI;
use dm870::uart::fifo;
use std::collections::VecDeque;
use std::vec::Vec;

#[derive(Default)]
struct Fake {
    rx: VecDeque<u8>,
    tx: Vec<u8>,
    req: bool,
    regs: std::collections::HashMap<u32, u32>,
}

impl Mmio for Fake {
    fn r32(&mut self, pa: u32) -> u32 {
        match pa {
            a if a == SPI + fifo::RX_FILL => self.rx.len() as u32,
            a if a == SPI + fifo::TX_FREE => 63,
            a => *self.regs.get(&a).unwrap_or(&0),
        }
    }
    fn w32(&mut self, pa: u32, v: u32) {
        if v & Pin::HostNireq.bit() != 0 {
            if pa == gpio::BASE + gpio::reg::DRIVE {
                self.req = true;
            } // driven (high: B set first)
            if pa == gpio::BASE + gpio::reg::RELEASE {
                self.req = false;
            } // let go: the pull-down wins
        }
        self.regs.insert(pa, v);
    }
    fn r8(&mut self, pa: u32) -> u8 {
        assert_eq!(pa, SPI + fifo::RX_DATA);
        self.rx.pop_front().unwrap()
    }
    fn w8(&mut self, pa: u32, v: u8) {
        assert_eq!(pa, SPI + fifo::TX_DATA);
        self.tx.push(v);
    }
}

/// What the MCU reads back: the reply without the idle bytes in front of it (the fake's TX FIFO is always empty, so
/// every reply has one).
fn ask(l: &mut Link<Fake>, bytes: &[u8]) -> Vec<u8> {
    let mut t = ask_raw(l, bytes);
    let idle = t.iter().take_while(|&&b| b == 0xFF).count();
    t.drain(..idle);
    t
}

fn ask_raw(l: &mut Link<Fake>, bytes: &[u8]) -> Vec<u8> {
    l.mmio().rx.extend(bytes);
    l.poll();
    core::mem::take(&mut l.mmio().tx)
}

#[test]
fn a_reply_into_an_empty_tx_fifo_goes_behind_an_idle_byte() {
    // the first byte into an empty FIFO can come out with bit 0 set on the unit: the echo's 02 read as 03
    let mut l = Link::new(Fake::default());
    assert_eq!(ask_raw(&mut l, &[0x02, 0x45, 1, 0, 0x51]), [0xFF, 0x02, 0x45, 0, 0]);
}

#[test]
fn status_ready_then_request_line_drops() {
    let mut l = Link::new(Fake::default());
    l.poll();
    assert!(l.mmio().req, "request line high until the MCU has read the status");
    assert_eq!(ask(&mut l, &[0xFF, 0x01, 0x11, 0, 0]), [0x01, 0x11, 0x21, 0x00]);
    assert!(!l.mmio().req);
}

#[test]
fn a_set_status_is_what_0x11_answers_and_is_pending_again() {
    let mut l = Link::new(Fake::default());
    l.set_status(status::BOOTING);
    assert_eq!(ask(&mut l, &[0xFF, 0x01, 0x11, 0, 0]), [0x01, 0x11, 0x20, 0x00]);
    l.set_status(status::READY);
    l.poll();
    assert!(l.mmio().req, "a changed status is pending");
    assert_eq!(ask(&mut l, &[0xFF, 0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x11, 0x00]);
    assert_eq!(ask(&mut l, &[0xFF, 0x01, 0x11, 0, 0]), [0x01, 0x11, 0x21, 0x00]);
}

#[test]
fn pending_names_status_first() {
    let mut l = Link::new(Fake::default());
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x11, 0x00]);
    ask(&mut l, &[0x01, 0x11, 0, 0]);
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0, 0]);
}

#[test]
fn stuffing_both_ways() {
    let mut l = Link::new(Fake::default());
    // write F reg 0x30 = 0xFF27: data bytes arrive stuffed, the echo goes back stuffed
    assert_eq!(ask(&mut l, &[0x00, 0x30, 0x27, 0x01, 0x27, 0x27]), [0x00, 0x30, 0x27, 0x01, 0x27, 0x27]);
    assert_eq!(l.reg(0x30), 0xFF27);
}

#[test]
fn hcr_0x21_requests_bl2() {
    let mut l = Link::new(Fake::default());
    assert!(!l.bl2_requested());
    ask(&mut l, &[0x00, reg::HCR, 0x21, 0x00]);
    assert!(l.bl2_requested());
}

#[test]
fn v_write_payload_is_consumed() {
    let mut l = Link::new(Fake::default());
    assert_eq!(ask(&mut l, &[0x02, 0x40, 3, 0, 1, 0x27, 0x27, 2]), [0x02, 0x40, 0, 0]);
    assert_eq!(ask(&mut l, &[0x01, 0x11, 0, 0]), [0x01, 0x11, 0x21, 0x00], "back in sync after the payload");
}

#[test]
fn notifications_queue_in_order_and_are_read_once() {
    let mut l = Link::new(Fake::default());
    ask(&mut l, &[0x01, 0x11, 0, 0]);
    l.notify(0x32, b"21");
    l.notify(0x38, b"192.0.2.123\0");
    l.poll();
    assert!(l.mmio().req);
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x32, 0]);
    assert_eq!(ask(&mut l, &[0x03, 0x32, 0, 0]), [0x03, 0x32, 2, 0, b'2', b'1']);
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x38, 0]);
    let r = ask(&mut l, &[0x03, 0x38, 0, 0]);
    assert_eq!(&r[2..4], &[12, 0]);
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0, 0]);
    assert!(!l.mmio().req, "request line drops once everything is read");
    // a direct read still answers with the last value
    assert_eq!(ask(&mut l, &[0x03, 0x32, 0, 0]), [0x03, 0x32, 2, 0, b'2', b'1']);
}

#[test]
fn a_request_half_received_is_dropped_and_the_next_one_parses() {
    let mut l = Link::new(Fake::default());
    // a V write of 100 bytes whose data was lost after 10: the parser waits for the other 90
    assert!(ask(&mut l, &[0x02, 0xC3, 100, 0, b'v', b'e', b'r', b's', b'i', b'o', b'n', b' ', b'1', b'2']).is_empty());
    assert!(l.rx_busy(), "half received");
    // without the drop, this read would be swallowed as data
    l.drop_partial();
    assert!(!l.rx_busy());
    assert_eq!(ask(&mut l, &[0x01, 0x11, 0, 0]), [0x01, 0x11, 0x21, 0x00]);
    // a whole V write is echoed: the MCU's acknowledgement
    assert_eq!(ask(&mut l, &[0x02, 0xC3, 2, 0, b'o', b'k']), [0x02, 0xC3, 0, 0]);
}

#[test]
fn long_notifications_and_a_full_queue() {
    let mut l = Link::new(Fake::default());
    let xml = [b'x'; 256];
    assert!(l.notify(0x47, &xml));
    assert!(!l.notify(0x47, &[0u8; NOTE_MAX + 1]), "longer than one V read");
    for r in 1..16u8 {
        assert!(l.notify(r, &[5, 0]));
    }
    assert!(!l.notify(0x99, &[5, 0]), "full of other registers");
    assert!(l.notify(0x05, &[6, 0]), "a register already queued: replaced in place");
    ask(&mut l, &[0x01, 0x11, 0, 0]);
    let r = ask(&mut l, &[0x03, 0x47, 0, 0]);
    assert_eq!(&r[..4], &[0x03, 0x47, 0, 1]);
    assert_eq!(r.len(), 4 + 256);
}

#[test]
fn mcu_writes_become_events() {
    let mut l = Link::new(Fake::default());
    ask(&mut l, &[0x00, 0x48, 0xDF, 0x00]);
    ask(&mut l, &[0x02, 0x40, 4, 0, 0x03, 0x03, 0x01, 0x27, 0x01]); // 03 03 01 FF (stuffed)
    let e = l.next_event().unwrap();
    assert_eq!((e.reg, e.v, e.value), (0x48, false, 0xDF00));
    let e = l.next_event().unwrap();
    assert_eq!((e.reg, e.v, e.payload()), (0x40, true, &[0x03, 0x03, 0x01, 0xFF][..]));
    assert_eq!(l.next_event(), None);
}

#[test]
fn f_read_takes_a_notification_too() {
    // the stock MCU firmware: 0x0C names 0x32, then an F read of 0x32
    let mut l = Link::new(Fake::default());
    ask(&mut l, &[0x01, 0x11, 0, 0]);
    l.notify(0x32, b"21");
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x32, 0]);
    assert_eq!(ask(&mut l, &[0x01, 0x32, 0, 0]), [0x01, 0x32, b'2', b'1']);
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0, 0], "taken: no loop");
    assert_eq!(ask(&mut l, &[0x01, 0x32, 0, 0]), [0x01, 0x32, b'2', b'1'], "the last value stays readable");
}

#[test]
fn headers_are_stuffed_both_ways() {
    // the stock MCU firmware reads V 0x27 (VersionCNE): request 03 [27 27] 00 00, reply 03 [27 27] len ...
    let mut l = Link::new(Fake::default());
    ask(&mut l, &[0x01, 0x11, 0, 0]);
    l.set_value(0x27, b"C1");
    assert_eq!(ask(&mut l, &[0x03, 0x27, 0x27, 0, 0]), [0x03, 0x27, 0x27, 2, 0, b'C', b'1']);
}

/// The stock MCU firmware's side of a read (protocol framing): request, then clock until the reply is complete.
fn stock_read(l: &mut Link<Fake>, reg: u8) -> std::vec::Vec<u8> {
    use zd1_spi::framing::{self, READ_F, READ_V, Reply, ReplyParser};
    use zd1_spi::spec::{Kind, kind};
    let typ = if kind(reg) == Kind::F { READ_F } else { READ_V };
    let mut req = std::vec::Vec::new();
    framing::request(typ, reg, 0, 0, &mut |b| req.push(b));
    l.mmio().rx.extend(req);
    l.poll();
    let reply = core::mem::take(&mut l.mmio().tx);
    let mut p = ReplyParser::<512>::new(typ, reg);
    for b in reply {
        match p.push(b) {
            Reply::F(v) => return v.to_be_bytes().to_vec(),
            Reply::V(n) => return p.data[..n].to_vec(),
            Reply::Pending => {}
        }
    }
    panic!("no complete reply to a read of 0x{reg:02x}");
}

#[test]
fn stock_mcu_drain_takes_every_notification_its_own_way() {
    // what zd1-linkd publishes at start-up (reg, data), read back by the stock MCU's drain: 0x0C names the register,
    // the MCU reads it F or V by its own table (0x32 / 0x37 F, the rest V). Every notification once, then 0x0C = 0.
    let notes: &[(u8, &[u8])] = &[
        (0x58, b"Zeppelin Air 000042"),
        (0x47, b"<update id=\"status\"><icon id=\"play\">em</icon></update>"),
        (0x32, b"21"),
        (0x37, b"1\0"),
        (0x38, b"192.168.50.101\0"),
        (0xE3, &[5, 0]),
    ];
    let mut l = Link::new(Fake::default());
    assert_eq!(stock_read(&mut l, 0x11), [0x21, 0x00]);
    for (r, d) in notes {
        assert!(l.notify(*r, d));
    }
    let mut got = std::vec::Vec::new();
    for _ in 0..20 {
        let p = stock_read(&mut l, 0x0C)[0];
        if p == 0 {
            break;
        }
        got.push((p, stock_read(&mut l, p)));
    }
    assert_eq!(got.len(), notes.len(), "every notification once, no read loop: {got:02x?}");
    for ((r, d), (gr, gd)) in notes.iter().zip(&got) {
        assert_eq!(r, gr);
        let want = if zd1_spi::spec::kind(*r) == zd1_spi::spec::Kind::F { &d[..2] } else { &d[..] };
        assert_eq!(&gd[..], want, "0x{r:02x}");
    }
}

#[test]
fn the_escape_byte_as_a_register_number() {
    let mut l = Link::new(Fake::default());
    l.set_value(0x27, b"C 2014092908219112\0");
    stock_read(&mut l, 0x11);
    assert_eq!(stock_read(&mut l, 0x27), b"C 2014092908219112\0", "0x27 is the escape byte: stuffed as a number too");
}

#[test]
fn a_newer_notification_replaces_the_queued_one() {
    let mut l = Link::new(Fake::default());
    ask(&mut l, &[0x01, 0x11, 0, 0]);
    l.notify(0x47, b"em");
    l.notify(0x32, b"21");
    l.notify(0x47, b"pl"); // the play state changed before the MCU read it
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x47, 0], "kept its place in the queue");
    assert_eq!(ask(&mut l, &[0x03, 0x47, 0, 0]), [0x03, 0x47, 2, 0, b'p', b'l']);
    assert_eq!(ask(&mut l, &[0x01, 0x0C, 0, 0]), [0x01, 0x0C, 0x32, 0]);
}
