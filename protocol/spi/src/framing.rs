//! How the protocol goes over the wire. SPI mode 3, the MCU is master; both directions are a byte stream in which
//! **every** byte is stuffed: 0x27 -> `27 27`, 0xFF -> `27 01`, so a raw 0xFF is always idle (nothing to say). The
//! stock firmware's writer stuffs whole messages and its reader unstuffs every byte (0x8000c050 / 0x8000c154) [RE].
//!
//! Messages (unstuffed):
//!   MCU -> module:  `00 reg b0 b1` write F, `01 reg 00 00` read F, `02 reg lenLo lenHi data` write V,
//!                   `03 reg 00 00` read V
//!   module -> MCU:  every request is answered `type reg` + payload: F `b0 b1`, V `lenLo lenHi data`, writes echo
//!                   (`00 reg b0 b1`, `02 reg 00 00`). The MCU skips to the type byte, then the register byte, of a
//!                   read's reply and never reads write echoes [RE].

pub use crate::spec::wire::{ESC, IDLE, READ_F, READ_V, WRITE_F, WRITE_V};
/// The longest V write the stock MCU sends [RE 0x8000b910].
pub const V_MAX: usize = crate::spec::wire::V_MAX as usize;

/// Where encoded bytes go. Use one named type per program (a TX buffer): every closure is a type of its own, and a
/// generic encoder is compiled once per sink type, so closures at many call sites would copy the encoders. Closures
/// still work (tests, host tools).
pub trait Sink {
    fn put(&mut self, b: u8);
}
impl<F: FnMut(u8)> Sink for F {
    #[inline]
    fn put(&mut self, b: u8) {
        self(b)
    }
}

/// Stuff one byte.
// not inlined: one copy per sink type, called from every encoder (at opt-level "s" an inlined copy at each of
// them is bigger than the call)
#[inline(never)]
pub fn stuff(b: u8, out: &mut impl Sink) {
    match b {
        ESC => {
            out.put(ESC);
            out.put(ESC)
        }
        IDLE => {
            out.put(ESC);
            out.put(0x01)
        }
        _ => out.put(b),
    }
}

/// Unstuffing, one raw byte at a time.
#[derive(Default)]
pub struct Unstuff {
    esc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rx {
    Idle,
    Escape,
    Byte(u8),
}

impl Unstuff {
    #[inline]
    pub fn push(&mut self, raw: u8) -> Rx {
        if self.esc {
            self.esc = false;
            return Rx::Byte(if raw == 0x01 { IDLE } else { raw });
        }
        match raw {
            IDLE => Rx::Idle,
            ESC => {
                self.esc = true;
                Rx::Escape
            }
            b => Rx::Byte(b),
        }
    }
    pub fn reset(&mut self) {
        self.esc = false;
    }
}

/// A request from the MCU. A V write's data is cut at the parser's buffer size; `len` is what was sent.
#[derive(Debug, PartialEq, Eq)]
pub enum Request<'a> {
    WriteF { reg: u8, value: u16 },
    ReadF { reg: u8 },
    WriteV { reg: u8, len: u16, data: &'a [u8] },
    ReadV { reg: u8 },
}

/// The module side: raw bytes from the MCU in, requests out. N = how much of a V write's data is kept.
pub struct RequestParser<const N: usize> {
    un: Unstuff,
    hdr: [u8; 4],
    n: usize,
    vleft: usize,
    vlen: u16,
    buf: [u8; N],
}

impl<const N: usize> Default for RequestParser<N> {
    fn default() -> Self {
        RequestParser { un: Unstuff::default(), hdr: [0; 4], n: 0, vleft: 0, vlen: 0, buf: [0; N] }
    }
}

impl<const N: usize> RequestParser<N> {
    /// Mid-message (a request half received): the caller should keep polling.
    pub fn busy(&self) -> bool {
        self.n > 0 || self.vleft > 0
    }

    /// Drop a request half received: bytes were lost (the RX FIFO overran while the caller was away), and what was
    /// taken for a V write's length would swallow every request after it. The MCU, missing the write's echo, sends
    /// it again once the line has been quiet.
    pub fn reset(&mut self) {
        self.n = 0;
        self.vleft = 0;
        self.un = Unstuff::default();
    }

    #[inline]
    pub fn push(&mut self, raw: u8) -> Option<Request<'_>> {
        let b = match self.un.push(raw) {
            Rx::Byte(b) => b,
            Rx::Idle | Rx::Escape => return None,
        };
        if self.vleft > 0 {
            let k = self.vlen as usize - self.vleft;
            if k < N {
                self.buf[k] = b;
            }
            self.vleft -= 1;
            return if self.vleft == 0 { Some(self.v_write()) } else { None };
        }
        self.hdr[self.n] = b;
        self.n += 1;
        if self.n < 4 {
            return None;
        }
        self.n = 0;
        let (typ, reg, b0, b1) = (self.hdr[0], self.hdr[1], self.hdr[2], self.hdr[3]);
        match typ {
            WRITE_F => Some(Request::WriteF { reg, value: (b0 as u16) << 8 | b1 as u16 }),
            READ_F => Some(Request::ReadF { reg }),
            READ_V => Some(Request::ReadV { reg }),
            WRITE_V => {
                self.vlen = b0 as u16 | (b1 as u16) << 8;
                self.vleft = self.vlen as usize;
                if self.vleft == 0 { Some(self.v_write()) } else { None }
            }
            _ => None, // not a request: ignore
        }
    }

    fn v_write(&self) -> Request<'_> {
        Request::WriteV { reg: self.hdr[1], len: self.vlen, data: &self.buf[..(self.vlen as usize).min(N)] }
    }
}

/// Module -> MCU: the reply to an F read (or the echo of an F write: `typ` = WRITE_F).
pub fn reply_f(typ: u8, reg: u8, value: u16, out: &mut impl Sink) {
    for b in [typ, reg, (value >> 8) as u8, value as u8] {
        stuff(b, out);
    }
}

/// Module -> MCU: the reply to a V read.
pub fn reply_v(reg: u8, data: &[u8], out: &mut impl Sink) {
    let n = data.len() as u16;
    for b in [READ_V, reg, n as u8, (n >> 8) as u8] {
        stuff(b, out);
    }
    for &b in data {
        stuff(b, out);
    }
}

/// Module -> MCU: the reply to a V read, its `n` data bytes pulled from `next` (a ring buffer, say: no copy).
pub fn reply_v_with(reg: u8, n: u16, mut next: impl FnMut() -> u8, out: &mut impl Sink) {
    for b in [READ_V, reg, n as u8, (n >> 8) as u8] {
        stuff(b, out);
    }
    for _ in 0..n {
        stuff(next(), out);
    }
}

/// MCU -> module: a request header (reads: b0 = b1 = 0), or a whole F write.
pub fn request(typ: u8, reg: u8, b0: u8, b1: u8, out: &mut impl Sink) {
    for b in [typ, reg, b0, b1] {
        stuff(b, out);
    }
}

/// MCU -> module: a V write.
pub fn write_v(reg: u8, data: &[u8], out: &mut impl Sink) {
    let n = data.len() as u16;
    for b in [WRITE_V, reg, n as u8, (n >> 8) as u8] {
        stuff(b, out);
    }
    for &b in data {
        stuff(b, out);
    }
}

/// The MCU side of a read: raw bytes clocked from the module in (after the 5 ms gap), the reply out. Skips to the
/// type byte, then to the register byte, as the stock firmware does (0x8000c2d4) [RE].
pub struct ReplyParser<const N: usize> {
    un: Unstuff,
    typ: u8,
    reg: u8,
    state: ReplyState,
    first: u8,
    len: usize,
    got: usize,
    pub data: [u8; N],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplyState {
    Type,
    Reg,
    First,
    Second,
    Data,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Pending,
    F(u16),
    V(usize),
}

impl<const N: usize> ReplyParser<N> {
    pub fn new(typ: u8, reg: u8) -> Self {
        ReplyParser {
            un: Unstuff::default(),
            typ,
            reg,
            state: ReplyState::Type,
            first: 0,
            len: 0,
            got: 0,
            data: [0; N],
        }
    }

    /// `Reply::V(n)`: `n` bytes were sent, the first N of them are in `data`.
    pub fn push(&mut self, raw: u8) -> Reply {
        let b = match self.un.push(raw) {
            Rx::Byte(b) => b,
            _ => return Reply::Pending,
        };
        match self.state {
            ReplyState::Type => {
                if b == self.typ {
                    self.state = ReplyState::Reg
                }
            }
            ReplyState::Reg => {
                if b == self.reg {
                    self.state = ReplyState::First
                }
            }
            ReplyState::First => {
                self.first = b;
                self.state = ReplyState::Second;
            }
            ReplyState::Second => {
                if self.typ == READ_F {
                    return Reply::F((self.first as u16) << 8 | b as u16);
                }
                self.len = self.first as usize | (b as usize) << 8;
                if self.len == 0 {
                    return Reply::V(0);
                }
                self.state = ReplyState::Data;
            }
            ReplyState::Data => {
                if self.got < N {
                    self.data[self.got] = b;
                }
                self.got += 1;
                if self.got == self.len {
                    return Reply::V(self.len);
                }
            }
        }
        Reply::Pending
    }
}
