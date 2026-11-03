//! The host link to our MCU firmware (docs/protocol/host.md): a request is a CALL with a sequence number; the MCU
//! runs it from its main loop and queues the reply behind the log text printed before it; POLL fetches both. HELLO
//! answers from the interrupt (it works while the main loop is busy). Requests that change the speaker are gated:
//! LOCKED asks for a press of ON/OFF on the speaker (`Unlock`) and sends the request again.

use crate::codec::{self, Msg, Request, Typed, encode_msg};
use crate::spec::{self, usb};
use crate::spec::{Err, msg};
use crate::usb::{Bus, Iface, Setup, Usb, UsbError};
use crate::{Error, Events, Result};
use std::rc::Rc;

/// bmRequestType: vendor, to the interface.
const VENDOR_IN: u8 = 0xC1;
const VENDOR_OUT: u8 = 0x41;

/// The host link's interface, found by its class: its number moves with the interfaces before it.
pub const IFACE: Iface = Iface::Class(usb::CLASS, usb::SUBCLASS, usb::PROTOCOL);

/// Our MCU firmware on USB.
pub struct Mcu<U: Usb> {
    pub usb: U,
    /// What it said at the start: the firmware, its protocol, its requests.
    pub hello: msg::HelloReply,
    /// The firmware speaks another host protocol: only the base layer (`Hello`, `Dfu`) goes through.
    pub foreign: bool,
    ev: Rc<dyn Events>,
    seq: u16,
    /// Replies by sequence number, from when they came: a call's own, or one whose call gave up waiting (dropped after
    /// [`STALE_MS`]). One that didn't decode is its call's error.
    pending: Vec<(u16, f64, std::result::Result<Msg, codec::Error>)>,
    /// The module's console bytes POLL brought (`Console` open), until [`Mcu::console_take`].
    console: Vec<u8>,
}

/// The most console bytes kept for [`Mcu::console_take`]: older ones go.
const CONSOLE_KEEP: usize = 1 << 20;

/// How long a reply nobody took is kept.
const STALE_MS: f64 = 60_000.0;

impl<U: Usb> Mcu<U> {
    /// Talk to `usb` (our firmware's device, its interface claimed): HELLO, the protocol checked. `backlog`: hand the
    /// log from before (up to 4 KB) to the events; otherwise it is dropped.
    pub async fn new(usb: U, ev: Rc<dyn Events>, backlog: bool) -> Result<Self> {
        let h = usb
            .control_in(
                Setup { bm: VENDOR_IN, request: usb::HELLO, value: 0, index: usb.iface() as u16 },
                spec::MSG_MAX as u16,
                2000,
            )
            .await
            .map_err(|e| {
                Error::Device(format!(
                    "the device on {:04X}:{:04X} does not answer the host link ({e}): firmware \
                                                from before the host link? DFU still works",
                    usb::VID,
                    usb::PID
                ))
            })?;
        let hello = typed::<msg::HelloReply>(codec::decode(&h)?)?;
        // another protocol: the base layer still works (HELLO from the interrupt, and `Dfu`, id 0x06 since the
        // first version), so a newer tool can always put an older firmware into the bootloader and flash it
        let foreign = hello.proto != spec::VERSION;
        let mut m =
            Mcu { usb, hello, foreign, ev, seq: crate::seed() as u16, pending: Vec::new(), console: Vec::new() };
        if !backlog {
            let keep = std::mem::replace(&mut m.ev, Rc::new(crate::Quiet));
            while m.poll().await? > 0 {}
            m.ev = keep;
        }
        Ok(m)
    }

    /// Our firmware on `bus` (None: not on USB as 03EB:2FE0).
    pub async fn open<B: Bus<Dev = U>>(bus: &B, ev: Rc<dyn Events>, backlog: bool) -> Result<Option<Self>> {
        match bus.open(usb::VID, usb::PID, IFACE).await? {
            Some(d) => Ok(Some(Mcu::new(d, ev, backlog).await?)),
            None => Ok(None),
        }
    }

    pub fn events(&self) -> &Rc<dyn Events> {
        &self.ev
    }

    /// The firmware has this request (`HelloReply.requests`).
    pub fn supports(&self, name: &str) -> bool {
        let Some(m) = spec::message(name) else { return false };
        self.hello.requests.get(m.id as usize / 8).is_some_and(|b| b >> (m.id % 8) & 1 == 1)
    }

    /// One POLL: the log text to the events, replies kept by sequence number (a reply that doesn't decode goes to its
    /// call as the error). The number of records.
    pub async fn poll(&mut self) -> Result<usize> {
        let d = self
            .usb
            .control_in(
                Setup { bm: VENDOR_IN, request: usb::POLL, value: 0, index: self.usb.iface() as u16 },
                spec::POLL_MAX as u16,
                2000,
            )
            .await
            .map_err(|e| Error::Usb("POLL", e))?;
        let (mut i, mut n) = (0, 0);
        while i + 3 <= d.len() {
            let (kind, len) = (d[i], u16::from_le_bytes([d[i + 1], d[i + 2]]) as usize);
            let body = &d[i + 3..(i + 3 + len).min(d.len())];
            i += 3 + len;
            if kind == spec::record::TEXT && !body.is_empty() {
                self.ev.text(body);
                n += 1;
            } else if kind == spec::record::CONSOLE && !body.is_empty() {
                self.console.extend_from_slice(body);
                let over = self.console.len().saturating_sub(CONSOLE_KEEP);
                self.console.drain(..over);
                n += 1;
            } else if kind == spec::record::REPLY && body.len() >= 2 {
                let now = crate::now_ms();
                self.pending.retain(|p| now - p.1 < STALE_MS);
                self.pending.push((u16::from_le_bytes([body[0], body[1]]), now, codec::decode(&body[2..])));
                n += 1;
            }
        }
        Ok(n)
    }

    /// Send a request and wait for its reply; an `Error` reply is [`Error::Mcu`]. A LOCKED answer asks the person
    /// for a press on the speaker (`Unlock`) and sends it again.
    pub async fn call<R: Request>(&mut self, r: &R) -> Result<R::Reply> {
        typed(self.call_msg(R::NAME, &encode_msg(r)?, true).await?)
    }

    /// A request already encoded (`name` its message), the reply as decoded: for front-ends that take requests by
    /// name (the web page). It waits as long as the spec says for that request.
    pub async fn call_msg(&mut self, name: &str, request: &[u8], unlock: bool) -> Result<Msg> {
        if self.foreign && name != "Dfu" && name != "Hello" {
            return Err(Error::Device(format!(
                "the firmware ({}) speaks host protocol {}, this program {}: flash a matching \
                                              build (`zd1 flash`, `zd1 install` work across versions)",
                self.hello.fw,
                self.hello.proto,
                spec::VERSION
            )));
        }
        if !self.supports(name) {
            return Err(Error::Mcu(
                Err::Unknown,
                format!("this firmware ({}) has no {name}: flash a newer build", self.hello.fw),
            ));
        }
        match self.call_once(name, request).await {
            Err(Error::Mcu(Err::Locked, _)) if unlock => {}
            r => return r,
        }
        // the firmware waits 30 s for the press; a patient program (a person at it) asks again until they press or
        // stop it
        loop {
            self.ev.prompt(&format!(
                "{name} changes the speaker: press ON/OFF on it within 30 s to allow it (its LED \
                                     flashes purple)"
            ));
            match self.call_once("Unlock", &encode_msg(&msg::Unlock)?).await {
                Err(Error::Mcu(Err::Denied, _)) if self.ev.patient() && !self.ev.cancelled() => continue,
                Err(Error::Mcu(Err::Denied, _)) if self.ev.cancelled() => return Err(Error::Cancelled),
                r => {
                    r?;
                }
            }
            return self.call_once(name, request).await;
        }
    }

    async fn call_once(&mut self, name: &str, request: &[u8]) -> Result<Msg> {
        let def = spec::message(name).ok_or_else(|| Error::Codec(codec::Error::UnknownMessage(name.into())))?;
        let timeout = def.timeout(&codec::decode(request)?.vals);
        // the firmware's waits are in its time (the emulator's runs slower than ours)
        let timeout = (timeout * self.usb.slow()) as f64;
        self.seq = self.seq.wrapping_add(1);
        let (seq, t0) = (self.seq, crate::now_ms());
        loop {
            // CALL stalls while an earlier request still runs
            let s = Setup { bm: VENDOR_OUT, request: usb::CALL, value: seq, index: self.usb.iface() as u16 };
            match self.usb.control_out(s, request, 2000).await {
                Ok(()) => break,
                Err(UsbError::Gone) => return Err(Error::Usb("CALL", UsbError::Gone)),
                Err(_) if crate::now_ms() - t0 > timeout => {
                    return Err(Error::Timeout(format!("{name}: the MCU did not take the request (busy?)")));
                }
                Err(_) => {
                    self.poll().await?;
                    self.usb.sleep(50).await;
                }
            }
        }
        loop {
            if let Some(i) = self.pending.iter().position(|p| p.0 == seq) {
                let r = self.pending.remove(i).2?;
                if r.name() == "Error" {
                    let e = msg::Error::from_msg(&r);
                    return Err(Error::Mcu(e.code, e.text));
                }
                return Ok(r);
            }
            if crate::now_ms() - t0 > timeout {
                return Err(Error::Timeout(format!("{name}: no reply within {:.0} s", timeout / 1000.0)));
            }
            if self.poll().await? == 0 {
                self.usb.sleep(20).await;
            }
        }
    }

    /// `data` to the bulk pipe (the endpoint `usb::BULK_OUT`; progress as `what`), 64 KB at a time with `each(self, sent)` after each
    /// (false: stop there), then the zero-length packet the MCU needs when the data ends on a full packet inside its
    /// 512-byte buffer (protocol/host). The bytes sent.
    pub async fn bulk(
        &mut self,
        what: &str,
        data: &[u8],
        mut each: impl AsyncFnMut(&mut Self, usize) -> Result<bool>,
    ) -> Result<usize> {
        const CHUNK: usize = 64 * 1024;
        let mut sent = 0;
        while sent < data.len() {
            let n = CHUNK.min(data.len() - sent);
            sent += self
                .usb
                .bulk_out(usb::BULK_OUT, &data[sent..sent + n], 30_000)
                .await
                .map_err(|e| Error::Usb("bulk OUT", e))?;
            self.ev.progress(what, sent as u64, data.len() as u64);
            if !each(self, sent).await? {
                return Ok(sent);
            }
        }
        if data.len() % 64 == 0 && data.len() % 512 != 0 {
            self.usb
                .bulk_out(usb::BULK_OUT, &[], 10_000)
                .await
                .map_err(|e| Error::Usb("the closing zero-length packet", e))?;
        }
        Ok(sent)
    }

    /// Open the module's console for this host (afresh: [`Mcu::console_take`] starts empty) or close it.
    pub async fn console_open(&mut self, open: bool) -> Result<msg::ConsoleReply> {
        let r = self.call(&msg::Console { open }).await?;
        self.console.clear();
        Ok(r)
    }

    /// The module's console bytes POLL has brought since the last take.
    pub fn console_take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.console)
    }

    /// `data` to the module's console (open: [`Mcu::console_open`]): `ConsoleWrite`s until the MCU's send ring has
    /// taken all of it, polling between (the console's bytes keep coming); 5 s of the device's time without progress
    /// is an error.
    pub async fn console_write(&mut self, data: &[u8]) -> Result<()> {
        let (mut rest, mut t0) = (data, crate::now_ms());
        while !rest.is_empty() {
            let n = rest.len().min(240);
            let taken = self.call(&msg::ConsoleWrite { data: rest[..n].to_vec() }).await?.taken as usize;
            rest = &rest[taken.min(n)..];
            if taken > 0 {
                t0 = crate::now_ms();
            }
            if taken < n {
                if crate::now_ms() - t0 > (5000 * self.usb.slow()) as f64 {
                    return Err(Error::Timeout("the module's console took no more bytes for 5 s".into()));
                }
                self.poll().await?;
                self.usb.sleep(5).await;
            }
        }
        Ok(())
    }

    /// Hand the log to the events for `ms` (what a request printed after its reply).
    pub async fn follow_for(&mut self, ms: u32) -> Result<()> {
        let t0 = crate::now_ms();
        while crate::now_ms() - t0 < ms as f64 && !self.ev.cancelled() {
            if self.poll().await? == 0 {
                self.usb.sleep(50).await;
            }
        }
        Ok(())
    }

    /// Hand the log to the events until the person stops it.
    pub async fn follow(&mut self) -> Result<()> {
        while !self.ev.cancelled() {
            if self.poll().await? == 0 {
                self.usb.sleep(50).await;
            }
        }
        Ok(())
    }
}

/// A decoded message as the type expected.
fn typed<T: Typed>(m: Msg) -> Result<T> {
    if m.name() != T::NAME {
        return Err(Error::Device(format!("the MCU answered {} where {} was due", m.name(), T::NAME)));
    }
    Ok(T::from_msg(&m))
}
