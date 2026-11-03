//! The module side of the MCU <-> module SPI link: the slave half of the stock DM870 register protocol, so our MCU
//! firmware (and its DFU bootloader) talk to a module that runs our code instead of KnOS. `no_std`, no allocation. The protocol
//! itself (registers, codes, framing) is zd1-spi (protocol/spi); this crate is the module's behaviour on top of it:
//!   0x11 status   -> 0x2100 ("ready"), or what [`Link::set_status`] says (the Linux boot stub: 0x2000)
//!   0x0C pending  -> the register with queued data: the status until read, then notifications ([`Link::notify`],
//!                    oldest first; taken by the MCU's F or V read of that register)
//!   other reads   -> the last value set ([`Link::set_value`]) or written; writes come out of [`Link::next_event`]
//! The request line (DM870 GPIO 6) is high while anything is pending. Used by the bare-metal images (module/boot) and
//! zd1-linkd under Linux (module/os/apps/zd1-linkd); a board only says how to reach the registers ([`dm870::Mmio`]).
#![no_std]

pub use zd1_spi as spi;
pub use zd1_spi::spec::reg;

use dm870::Mmio;
use dm870::gpio::{Level, Pin, gpio};
use dm870::spi_slave::{TX_DEPTH, spi_slave};
use dm870::timers::timers;
use zd1_spi::framing::{self, READ_F, Request, RequestParser, WRITE_F, WRITE_V};
use zd1_spi::spec::status;

/// Longest notification payload: a document (the status document on 0xD6, a document read over 0x45); the stock
/// module's longest was 256 (DRC XML on 0x47, in pieces).
pub const NOTE_MAX: usize = 1024;
const NOTES: usize = 16;
const VALUE_MAX: usize = 48;
const VALUES: usize = 24; // zd1-linkd keeps ~16 (versions, name, MAC, play state, network,
// volume, config, DRC ack); when full, the last slot is reused
/// How much of a V write an event keeps: the configuration channel sends name + value in one write (our settings:
/// op, length, "zd1/" + a 32-byte key, a 128-byte value).
pub const EVENT_MAX: usize = 192;
/// What the MCU wrote: an F register (the 16-bit value) or a V register (the payload, up to EVENT_MAX bytes kept;
/// `len` is what was sent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub reg: u8,
    pub v: bool,
    pub value: u16,
    pub len: u16,
    pub data: [u8; EVENT_MAX],
}

impl Event {
    pub fn payload(&self) -> &[u8] {
        &self.data[..(self.len as usize).min(EVENT_MAX)]
    }
}

struct Note {
    reg: u8,
    len: u16,
    data: [u8; NOTE_MAX],
}

/// The reply being clocked out (stuffed), and where the TX FIFO has got to. One sink type for all encoders: each is
/// compiled once (framing::Sink).
struct Tx {
    buf: [u8; 2 * NOTE_MAX + 8],
    len: usize,
    pos: usize,
}

impl Tx {
    fn start(&mut self) -> &mut Self {
        self.len = 0;
        self.pos = 0;
        self
    }
}

impl framing::Sink for Tx {
    #[inline]
    fn put(&mut self, b: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }
}

pub struct Link<M: Mmio> {
    m: M,
    rx: RequestParser<EVENT_MAX>,
    tx: Tx,
    regs: [u16; 256],
    status: u16, // what 0x11 answers: READY unless set_status
    status_unread: bool,
    rx_count: u32,
    notes: [Note; NOTES],
    nh: usize,
    nn: usize,                                   // queued notifications, oldest first
    values: [(u8, u8, [u8; VALUE_MAX]); VALUES], // last V value per register (reg 0 = free slot)
    events: [Event; 16],
    eh: usize,
    en: usize,
    #[cfg(feature = "trace")]
    reqlog: [(u8, u8, u16); 32],
    #[cfg(feature = "trace")]
    reqs: usize, // the last requests (type, reg, value or length), for tracing
}

impl<M: Mmio> Link<M> {
    /// Takes the board, sets up the SPI slave (the stock init sequence) and drops the request line. The slave's pads
    /// and clock are the board's to set up first ([`dm870::pads::BL2`], [`dm870::clocks::Gates::SPI_SLAVE`]): the
    /// boot stub does it on bare metal, init under Linux.
    pub fn new(mut m: M) -> Self {
        spi_slave(&mut m).init();
        request_line(&mut m, false);
        const NOTE: Note = Note { reg: 0, len: 0, data: [0; NOTE_MAX] };
        const EVENT: Event = Event { reg: 0, v: false, value: 0, len: 0, data: [0; EVENT_MAX] };
        Link {
            m,
            rx: RequestParser::default(),
            tx: Tx { buf: [0; 2 * NOTE_MAX + 8], len: 0, pos: 0 },
            regs: [0; 256],
            status: status::READY,
            status_unread: true,
            rx_count: 0,
            notes: [NOTE; NOTES],
            nh: 0,
            nn: 0,
            values: [(0, 0, [0; VALUE_MAX]); VALUES],
            events: [EVENT; 16],
            eh: 0,
            en: 0,
            #[cfg(feature = "trace")]
            reqlog: [(0, 0, 0); 32],
            #[cfg(feature = "trace")]
            reqs: 0,
        }
    }

    /// What reg 0x11 answers from now on (default [`status::READY`]); the MCU is told again through 0x0C.
    pub fn set_status(&mut self, v: u16) {
        if v != self.status {
            self.status = v;
            self.status_unread = true;
        }
    }

    /// Queue a notification: the MCU is told (0x0C, request line) and reads `data` from register `reg`. Short values
    /// are also kept for direct reads. A notification still queued for the same register is replaced in place: they
    /// are all state (play state, network, volume, the ack of the one DRC in flight), the latest is what counts, and
    /// the queue can't fill up while the MCU is busy (a dropped "playing" kept the speaker from switching). False
    /// when the queue is full of other registers or `data` is too long.
    pub fn notify(&mut self, reg: u8, data: &[u8]) -> bool {
        if data.len() <= VALUE_MAX {
            self.set_value(reg, data);
        }
        if data.len() > NOTE_MAX {
            return false;
        }
        let queued = (0..self.nn).map(|k| (self.nh + k) % NOTES).find(|&i| self.notes[i].reg == reg);
        let slot = match queued {
            Some(i) => i,
            None if self.nn == NOTES => return false,
            None => {
                self.nn += 1;
                (self.nh + self.nn - 1) % NOTES
            }
        };
        let n = &mut self.notes[slot];
        n.reg = reg;
        n.len = data.len() as u16;
        n.data[..data.len()].copy_from_slice(data);
        true
    }

    /// The value a V read of `reg` returns while nothing is queued for it (up to 48 bytes; longer ones are cut).
    pub fn set_value(&mut self, reg: u8, data: &[u8]) {
        let n = data.len().min(VALUE_MAX);
        let i = self
            .values
            .iter()
            .position(|v| v.0 == reg && v.1 > 0)
            .or_else(|| self.values.iter().position(|v| v.1 == 0))
            .unwrap_or(VALUES - 1);
        self.values[i].0 = reg;
        self.values[i].1 = n as u8;
        self.values[i].2[..n].copy_from_slice(&data[..n]);
    }

    /// The next write from the MCU, oldest first (16 kept; older ones are dropped when the caller falls behind).
    pub fn next_event(&mut self) -> Option<Event> {
        if self.en == 0 {
            return None;
        }
        let e = self.events[self.eh];
        self.eh = (self.eh + 1) % self.events.len();
        self.en -= 1;
        Some(e)
    }

    fn push_event(&mut self, e: Event) {
        let n = self.events.len();
        if self.en == n {
            self.eh = (self.eh + 1) % n;
            self.en -= 1;
        }
        self.events[(self.eh + self.en) % n] = e;
        self.en += 1;
    }

    /// The board, for the caller's own register accesses (console UART, timestamp).
    pub fn mmio(&mut self) -> &mut M {
        &mut self.m
    }

    /// Last value the MCU wrote to an F register (type-0 write: b0 in the high byte).
    pub fn reg(&self, r: u8) -> u16 {
        self.regs[r as usize]
    }
    pub fn set_reg(&mut self, r: u8, v: u16) {
        self.regs[r as usize] = v;
    }
    /// Request bytes received so far (wrapping; idle 0xFF clocking doesn't count): whether the MCU is talking.
    pub fn activity(&self) -> u32 {
        self.rx_count
    }
    /// A request half received or a response still being clocked out: poll again soon.
    pub fn busy(&self) -> bool {
        self.rx.busy() || self.tx.pos < self.tx.len
    }
    /// A request from the MCU half received.
    pub fn rx_busy(&self) -> bool {
        self.rx.busy()
    }
    /// Drop it (nothing more came: its bytes were lost): the next byte starts a request again.
    pub fn drop_partial(&mut self) {
        self.rx.reset();
    }

    /// HCR Ctrl (reg 0x28) b0 = 0x21: the MCU asks for BL2 (the stock app's "start bootloaderL2 to update").
    pub fn bl2_requested(&self) -> bool {
        self.regs[reg::HCR as usize] >> 8 == 0x21
    }

    /// Let the request line go: before the chip is handed on (the kernel), whose link starts from nothing pending.
    pub fn release_request_line(&mut self) {
        request_line(&mut self.m, false);
    }

    /// Poll the link for `us` microseconds (the timestamp must run: [`dm870::timers::Timers::start_timestamp`]).
    pub fn serve_for(&mut self, us: u32) {
        let t0 = timers(&mut self.m).now();
        while timers(&mut self.m).us_since(t0) < us {
            self.poll();
        }
    }

    /// The stock "reboot into BL2" (docs/module/boot-chain.md): HCR status, 0.5 s for the MCU to drain our
    /// console, then the BL2 mailbox and the watchdog ([`dm870::reset_into_bl2`]).
    pub fn reboot_to_bl2(&mut self) -> ! {
        self.set_reg(reg::HCR_STATUS, 0x2020);
        self.serve_for(500_000);
        dm870::reset_into_bl2(&mut self.m)
    }

    /// Requests served so far (wrapping) and the last 32 of them: (type, register, F value / V length).
    #[cfg(feature = "trace")]
    pub fn requests(&self) -> (usize, &[(u8, u8, u16); 32]) {
        (self.reqs, &self.reqlog)
    }

    /// An F reply (or write echo) into the TX buffer, stuffed (framing.rs).
    fn reply_f(&mut self, typ: u8, reg: u8, v: u16) {
        framing::reply_f(typ, reg, v, self.tx.start());
    }

    fn read_f(&mut self, reg: u8) {
        let v = match reg {
            reg::STATUS => {
                self.status_unread = false;
                self.status
            }
            reg::PENDING => {
                if self.status_unread {
                    (reg::STATUS as u16) << 8
                } else if self.nn > 0 {
                    (self.notes[self.nh].reg as u16) << 8
                } else {
                    0
                }
            }
            // the stock MCU firmware reads short notifications (network status 0x32 "21") F-style
            _ if self.nn > 0 && self.notes[self.nh].reg == reg => {
                let n = &self.notes[self.nh];
                let v = (n.data[0] as u16) << 8 | if n.len > 1 { n.data[1] as u16 } else { 0 };
                self.nh = (self.nh + 1) % NOTES;
                self.nn -= 1;
                v
            }
            _ => match self.values.iter().find(|v| v.0 == reg && v.1 > 0) {
                Some(v) => (v.2[0] as u16) << 8 | if v.1 > 1 { v.2[1] as u16 } else { 0 },
                None => self.regs[reg as usize],
            },
        };
        self.reply_f(READ_F, reg, v);
    }

    /// A V read, encoded straight from where the data is (notification, value) into the TX buffer:
    /// the branches only pick the source, so the encoder exists once (opt-level "s").
    fn read_v(&mut self, reg: u8) {
        let Link { tx, notes, nh, nn, values, regs, status, status_unread, .. } = self;
        let w = tx.start();
        let word;
        let data: &[u8] = if reg == reg::STATUS {
            // V read of a pending status (the drain path)
            *status_unread = false;
            word = status.to_be_bytes();
            &word
        } else if *nn > 0 && notes[*nh].reg == reg {
            let note = &notes[*nh];
            *nh = (*nh + 1) % NOTES;
            *nn -= 1;
            &note.data[..note.len as usize]
        } else if let Some(v) = values.iter().find(|v| v.0 == reg && v.1 > 0) {
            &v.2[..v.1 as usize]
        } else {
            word = regs[reg as usize].to_be_bytes();
            &word
        };
        framing::reply_v(reg, data, w);
    }

    /// Service the SPI slave FIFOs: parse incoming bytes, keep the TX FIFO fed, set the request line. Call often.
    pub fn poll(&mut self) {
        while let Some(b) = spi_slave(&mut self.m).rx() {
            if b != framing::IDLE {
                self.rx_count = self.rx_count.wrapping_add(1);
            } // idle clocking isn't activity
            let (log, ev) = match self.rx.push(b) {
                None => continue,
                Some(Request::ReadF { reg }) => ((READ_F, reg, 0), None),
                Some(Request::ReadV { reg }) => ((framing::READ_V, reg, 0), None),
                Some(Request::WriteF { reg, value }) => {
                    ((WRITE_F, reg, value), Some(Event { reg, v: false, value, len: 0, data: [0; EVENT_MAX] }))
                }
                Some(Request::WriteV { reg, len, data }) => {
                    let mut e = Event { reg, v: true, value: 0, len, data: [0; EVENT_MAX] };
                    e.data[..data.len()].copy_from_slice(data);
                    ((WRITE_V, reg, len), Some(e))
                }
            };
            #[cfg(feature = "trace")]
            {
                self.reqlog[self.reqs % 32] = log;
                self.reqs = self.reqs.wrapping_add(1);
            }
            let (typ, reg, arg) = log;
            match typ {
                READ_F => self.read_f(reg),
                framing::READ_V => self.read_v(reg),
                WRITE_F => {
                    self.regs[reg as usize] = arg;
                    self.reply_f(WRITE_F, reg, arg);
                }
                _ => self.reply_f(WRITE_V, reg, 0),
            }
            if let Some(e) = ev {
                self.push_event(e);
            }
        }
        // a reply that starts into an empty TX FIFO goes behind an idle byte: the first byte the slave takes into an
        // empty FIFO while the MCU clocks can come out with bit 0 set (a V write's echo 02 read as 03 [HW 2026-11-03];
        // docs/protocol/sequences.md, Our OS, point 6). An idle byte stays one, and every reader skips it.
        if self.tx.pos == 0 && self.tx.len > 0 && spi_slave(&mut self.m).tx_free() >= TX_DEPTH {
            spi_slave(&mut self.m).tx(framing::IDLE);
        }
        // an escape pair (27 27 / 27 01) goes into the FIFO whole: split across two refills, the MCU can clock the
        // 0x27 and then the idle 0xFF, a broken escape (the stock firmware asserts in ercBcoSpiRead). Every byte is
        // stuffed, so walking from the start a 0x27 is always the first of a pair.
        let tx = &mut self.tx;
        while tx.pos < tx.len {
            let pair = tx.buf[tx.pos] == framing::ESC && tx.pos + 1 < tx.len;
            if pair && spi_slave(&mut self.m).tx_free() < 2 {
                break;
            }
            if !spi_slave(&mut self.m).tx(tx.buf[tx.pos]) {
                break;
            }
            tx.pos += 1;
            if pair {
                spi_slave(&mut self.m).tx(tx.buf[tx.pos]);
                tx.pos += 1;
            }
        }
        let want = self.status_unread || self.nn > 0;
        request_line(&mut self.m, want);
    }
}

#[cfg(test)]
mod tests;

/// The request line to the MCU: driven high while something is pending, else let go (the board's pull-down holds it
/// low; a pull-up alone loses to it, which is why "high" has to be driven).
fn request_line(m: &mut impl Mmio, high: bool) {
    if high { gpio(m).drive(Pin::HostNireq, Level::High) } else { gpio(m).release(Pin::HostNireq, true) }
}
