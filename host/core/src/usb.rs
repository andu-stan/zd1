//! The transport: control transfers to an interface, bulk OUT, and waits in the device's time. Two kinds of device:
//! a real one through nusb (macOS and Linux; WebUSB in the browser, the same code), and the emulator's USB
//! bridge (`ZD1DFU_EMU=host:port`: the USBB model's host side, emu/renode/avr32/UC3Usb.cs; EP0 only, and its time
//! runs slower than ours). A [`Bus`] finds a device by VID:PID and opens it with an interface claimed.

use crate::{Error, Result};
use std::fmt;

/// How a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbError {
    /// The device refused it (a STALL): a request it doesn't take now.
    Stall,
    /// The device left the bus (a reset into another firmware, unplugged).
    Gone,
    /// No completion in time.
    Timeout,
    /// The transport can't do it (bulk on the emulator's bridge).
    Unsupported,
    Other(String),
}

impl fmt::Display for UsbError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UsbError::Stall => f.write_str("stalled"),
            UsbError::Gone => f.write_str("the device left USB"),
            UsbError::Timeout => f.write_str("timed out"),
            UsbError::Unsupported => f.write_str("not on this transport"),
            UsbError::Other(s) => f.write_str(s),
        }
    }
}

/// A control request's setup: bmRequestType as on the wire (0x41 vendor OUT to an interface, 0xC1 vendor IN, 0x21
/// / 0xA1 class OUT / IN: DFU), bRequest, wValue, wIndex.
#[derive(Debug, Clone, Copy)]
pub struct Setup {
    pub bm: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

/// Which interface to claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Iface {
    /// By its number (the DFU bootloader's 0).
    Number(u8),
    /// The first with this class, subclass and protocol (the host link's: its number moves with the interfaces
    /// before it).
    Class(u8, u8, u8),
}

/// The number of the first interface of class/subclass/protocol `c` in a configuration descriptor (the whole of it).
pub fn find_class(config: &[u8], c: (u8, u8, u8)) -> Option<u8> {
    let mut i = 0;
    while i + 2 <= config.len() {
        let (len, kind) = (config[i] as usize, config[i + 1]);
        if len < 2 {
            return None;
        }
        if kind == 4 && len >= 9 && i + 9 <= config.len() && (config[i + 5], config[i + 6], config[i + 7]) == c {
            return Some(config[i + 2]);
        }
        i += len;
    }
    None
}

/// One opened USB device with its interface claimed.
#[allow(async_fn_in_trait)]
pub trait Usb {
    /// The claimed interface's number (`wIndex` of the requests to it).
    fn iface(&self) -> u8;
    async fn control_in(&self, s: Setup, len: u16, timeout_ms: u32) -> std::result::Result<Vec<u8>, UsbError>;
    async fn control_out(&self, s: Setup, data: &[u8], timeout_ms: u32) -> std::result::Result<(), UsbError>;
    /// A bulk OUT transfer (an empty one is a zero-length packet): the bytes written.
    async fn bulk_out(&self, ep: u8, data: &[u8], timeout_ms: u32) -> std::result::Result<usize, UsbError>;
    /// Wait in the device's time (the emulator's: virtual).
    async fn sleep(&self, ms: u32) {
        crate::sleep(ms).await
    }
    /// How much slower than ours the device's time runs (the emulator: ~10 with both boards linked); the firmware's
    /// own waits are in its time, so a host's timeouts stretch by this.
    fn slow(&self) -> u32 {
        1
    }
    /// EP0 only (the emulator's bridge): bulk data goes in requests instead (`PipeData`).
    fn ep0_only(&self) -> bool {
        false
    }
}

/// Where devices come from.
#[allow(async_fn_in_trait)]
pub trait Bus {
    type Dev: Usb;
    /// The device vid:pid with interface `iface` claimed, if it is on USB now (and has that interface) and this
    /// program may use it.
    async fn open(&self, vid: u16, pid: u16, iface: Iface) -> Result<Option<Self::Dev>>;
    /// Whether vid:pid is on USB now (one this program may see).
    async fn present(&self, vid: u16, pid: u16) -> Result<bool>;
    /// Wait in this bus's time (the emulator's: virtual).
    async fn sleep(&self, ms: u32) {
        crate::sleep(ms).await
    }
}

/// Wait up to `ms` of the bus's time for vid:pid to come, then open it. `ev` hears once that the device is wanted
/// (WebUSB: a device the page hasn't been granted yet; the person picks it in the browser's chooser). A device that
/// has just come may refuse the open for a moment while the host OS sets it up (WebUSB's open fails then): that is
/// tried again, and only the last refusal is the error, once the time is up. A patient program (`Events::patient`:
/// the page, a person at it) has no deadline once it asked for the device: the browser's chooser takes a person's
/// time, and while it is open the browser holds the device (an open fails: "an operation that changes interface state
/// is in progress"); it asks again each minute (a chooser closed without a pick), and stopping ends the wait.
pub async fn wait_open<B: Bus>(
    bus: &B,
    vid: u16,
    pid: u16,
    iface: Iface,
    ms: u32,
    ev: &dyn crate::Events,
    what: &str,
) -> Result<Option<B::Dev>> {
    const STEP: u32 = 200;
    const ASK_AGAIN: u32 = 60_000;
    let (mut waited, mut asked_at, mut refused) = (STEP, None::<u32>, None);
    bus.sleep(STEP).await; // what we wait for has just left, or is about to come
    loop {
        match bus.open(vid, pid, iface).await {
            Ok(Some(d)) => return Ok(Some(d)),
            Ok(None) => {}
            Err(e) => refused = Some(e),
        }
        if ev.cancelled() {
            return Err(Error::Cancelled);
        }
        if waited >= ms && !(asked_at.is_some() && ev.patient()) {
            return refused.map_or(Ok(None), Err);
        }
        if waited >= 1500 && asked_at.is_none_or(|t| waited - t >= ASK_AGAIN) {
            ev.wants_device(vid, pid, what);
            asked_at = Some(waited);
        }
        bus.sleep(STEP).await; // in the bus's time: the emulator's runs at its own pace
        waited += STEP;
    }
}

// ---- nusb: real USB, natively and through WebUSB ----------------------------------------------------------------

use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, Out, Recipient, TransferError};
use std::{cell::RefCell, time::Duration};

fn nusb_err(e: TransferError) -> UsbError {
    match e {
        TransferError::Stall => UsbError::Stall,
        TransferError::Disconnected => UsbError::Gone,
        TransferError::Cancelled => UsbError::Timeout,
        e => UsbError::Other(e.to_string()),
    }
}

/// An nusb operation to its end: awaited in the browser; natively it blocks (nusb's blocking calls want an async
/// runtime to be awaited, and a terminal program has no use for one).
async fn done<F: nusb::MaybeFuture>(f: F) -> F::Output {
    #[cfg(target_arch = "wasm32")]
    {
        f.await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        f.wait()
    }
}

fn kind(bm: u8) -> (ControlType, Recipient) {
    let t = match bm >> 5 & 3 {
        0 => ControlType::Standard,
        1 => ControlType::Class,
        _ => ControlType::Vendor,
    };
    let r = match bm & 0x1F {
        0 => Recipient::Device,
        1 => Recipient::Interface,
        2 => Recipient::Endpoint,
        _ => Recipient::Other,
    };
    (t, r)
}

/// A device through nusb, its interface claimed.
pub struct NusbDev {
    pub device: nusb::Device,
    pub intf: nusb::Interface,
    bulk: RefCell<Option<nusb::Endpoint<Bulk, Out>>>,
}

impl Usb for NusbDev {
    fn iface(&self) -> u8 {
        self.intf.interface_number()
    }

    async fn control_in(&self, s: Setup, len: u16, timeout_ms: u32) -> std::result::Result<Vec<u8>, UsbError> {
        let (control_type, recipient) = kind(s.bm);
        let c = ControlIn { control_type, recipient, request: s.request, value: s.value, index: s.index, length: len };
        done(self.intf.control_in(c, Duration::from_millis(timeout_ms as u64))).await.map_err(nusb_err)
    }

    async fn control_out(&self, s: Setup, data: &[u8], timeout_ms: u32) -> std::result::Result<(), UsbError> {
        let (control_type, recipient) = kind(s.bm);
        let c = ControlOut { control_type, recipient, request: s.request, value: s.value, index: s.index, data };
        done(self.intf.control_out(c, Duration::from_millis(timeout_ms as u64))).await.map_err(nusb_err)
    }

    async fn bulk_out(&self, ep: u8, data: &[u8], timeout_ms: u32) -> std::result::Result<usize, UsbError> {
        let taken = self.bulk.borrow_mut().take();
        let mut e = match taken {
            Some(e) => e,
            None => self.intf.endpoint::<Bulk, Out>(ep).map_err(|e| UsbError::Other(e.to_string()))?,
        };
        e.submit(nusb::transfer::Buffer::from(data.to_vec()));
        #[cfg(not(target_arch = "wasm32"))]
        let c = match e.wait_next_complete(Duration::from_millis(timeout_ms as u64)) {
            Some(c) => c,
            None => {
                e.cancel_all();
                let _ = e.wait_next_complete(Duration::from_secs(1));
                *self.bulk.borrow_mut() = Some(e);
                return Err(UsbError::Timeout);
            }
        };
        #[cfg(target_arch = "wasm32")]
        let c = {
            // WebUSB can't cancel a transfer: on a timeout the endpoint goes (with the transfer still in the browser's
            // hands) and the next call opens it again
            use std::task::Poll;
            let mut timer = std::pin::pin!(crate::sleep(timeout_ms));
            let c = std::future::poll_fn(|cx| match e.poll_next_complete(cx) {
                Poll::Ready(c) => Poll::Ready(Some(c)),
                Poll::Pending => timer.as_mut().poll(cx).map(|_| None),
            })
            .await;
            match c {
                Some(c) => c,
                None => return Err(UsbError::Timeout),
            }
        };
        *self.bulk.borrow_mut() = Some(e);
        c.status.map_err(nusb_err)?;
        Ok(c.actual_len)
    }
}

/// Real USB through nusb. In the browser it sees the devices the page has been granted (WebUSB).
pub struct NusbBus;

impl Bus for NusbBus {
    type Dev = NusbDev;

    /// A refused open or claim is tried again for up to [`BUSY_MS`]: a program that just let go of the device (a
    /// flow's step closing it, the next one opening it) leaves the browser releasing its interface in the
    /// background, and WebUSB refuses the device meanwhile ("an operation that changes interface state is in
    /// progress"). A device that isn't there is None at once.
    async fn open(&self, vid: u16, pid: u16, iface: Iface) -> Result<Option<NusbDev>> {
        let t0 = crate::now_ms();
        loop {
            match self.open_once(vid, pid, iface).await {
                Err(_) if crate::now_ms() - t0 < BUSY_MS => crate::sleep(200).await,
                r => return r,
            }
        }
    }

    async fn present(&self, vid: u16, pid: u16) -> Result<bool> {
        let mut l = done(nusb::list_devices()).await.map_err(|e| Error::Device(e.to_string()))?;
        Ok(l.any(|d| d.vendor_id() == vid && d.product_id() == pid))
    }
}

/// How long a refused open is tried again ([`NusbBus`]'s `open`).
const BUSY_MS: f64 = 3000.0;

impl NusbBus {
    async fn open_once(&self, vid: u16, pid: u16, iface: Iface) -> Result<Option<NusbDev>> {
        let usb = |e: nusb::Error| Error::Device(format!("{vid:04X}:{pid:04X}: {e}"));
        let Some(info) =
            done(nusb::list_devices()).await.map_err(usb)?.find(|d| d.vendor_id() == vid && d.product_id() == pid)
        else {
            return Ok(None);
        };
        let device = done(info.open()).await.map_err(usb)?;
        if device.active_configuration().is_err() {
            // a device no driver configured (the DFU bootloader)
            let _ = done(device.set_configuration(1)).await;
        }
        let iface = match iface {
            Iface::Number(n) => n,
            Iface::Class(c, s, p) => {
                let n = device.active_configuration().ok().and_then(|cfg| {
                    cfg.interface_alt_settings()
                        .find(|a| (a.class(), a.subclass(), a.protocol()) == (c, s, p))
                        .map(|a| a.interface_number())
                });
                let Some(n) = n else { return Ok(None) };
                n
            }
        };
        let intf = done(device.claim_interface(iface)).await.map_err(|e| {
            Error::Device(format!(
                "the speaker's interface {iface} is in use by another program (a browser tab, another zd1 command): {e}"
            ))
        })?;
        Ok(Some(NusbDev { device, intf, bulk: RefCell::new(None) }))
    }
}

// ---- the emulator's USB bridge ---------------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub use emu::{EmuBus, EmuDev};

#[cfg(not(target_arch = "wasm32"))]
mod emu {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// The emulator's bridge (`zemu run --mcu`: 127.0.0.1:4620 with `--keep`, `ZD1DFU_EMU` in `--host` steps): one
    /// device, whichever firmware the emulated MCU runs. A STALL or a device reset answers neither A nor D; `R` is a bus
    /// reset.
    pub struct EmuDev {
        s: RefCell<TcpStream>,
        iface: u8,
    }

    impl EmuDev {
        fn x(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, UsbError> {
            let io = |e: std::io::Error| UsbError::Other(format!("emulator: {e}"));
            let mut s = self.s.borrow_mut();
            s.write_all(msg).map_err(io)?;
            let mut r = [0u8; 1];
            s.read_exact(&mut r).map_err(io)?;
            match r[0] {
                b'A' => Ok(Vec::new()),
                b'D' => {
                    let mut n = [0u8; 2];
                    s.read_exact(&mut n).map_err(io)?;
                    let mut d = vec![0u8; u16::from_le_bytes(n) as usize];
                    s.read_exact(&mut d).map_err(io)?;
                    Ok(d)
                }
                _ => Err(UsbError::Stall),
            }
        }

        fn setup(&self, s: Setup, wlen: u16, data: &[u8]) -> std::result::Result<Vec<u8>, UsbError> {
            let mut m = vec![b'S', s.bm, s.request];
            for v in [s.value, s.index, wlen] {
                m.extend(v.to_le_bytes());
            }
            m.extend(data);
            self.x(&m)
        }
    }

    impl Usb for EmuDev {
        fn iface(&self) -> u8 {
            self.iface
        }
        async fn control_in(&self, s: Setup, len: u16, _t: u32) -> std::result::Result<Vec<u8>, UsbError> {
            self.setup(s, len, &[])
        }
        async fn control_out(&self, s: Setup, data: &[u8], _t: u32) -> std::result::Result<(), UsbError> {
            self.setup(s, data.len() as u16, data).map(|_| ())
        }
        async fn bulk_out(&self, _ep: u8, _d: &[u8], _t: u32) -> std::result::Result<usize, UsbError> {
            Err(UsbError::Unsupported)
        }
        async fn sleep(&self, ms: u32) {
            // the model answers once that much virtual time has passed
            let _ = self.x(&[&b"Z"[..], &(ms * 1000).to_le_bytes()].concat());
        }
        fn slow(&self) -> u32 {
            10
        }
        fn ep0_only(&self) -> bool {
            true
        }
    }

    /// The bridge at `host:port`.
    pub struct EmuBus(pub String);

    impl EmuBus {
        /// `ZD1DFU_EMU`, if set.
        pub fn from_env() -> Option<EmuBus> {
            std::env::var("ZD1DFU_EMU").ok().filter(|s| !s.is_empty()).map(EmuBus)
        }

        /// A connection to the bridge; no bus reset yet.
        fn connect(&self) -> Result<EmuDev> {
            let s =
                TcpStream::connect(&self.0).map_err(|e| Error::Device(format!("emulator USB at {}: {e}", self.0)))?;
            Ok(EmuDev { s: RefCell::new(s), iface: 0 })
        }

        /// A bus reset (`R`), then the device's VID:PID from its descriptor. Our firmware answers only after a reset
        /// (as on a real bus, where the host resets what attaches); a prober waits between tries in the emulator's
        /// time ([`Bus::sleep`]), since resets a few emulated ms apart wedge our firmware while it enumerates.
        fn probe(&self) -> Result<Option<(EmuDev, (u16, u16))>> {
            let d = self.connect()?;
            // emulated time is not ours: the polled bootloader can leave a request pending for long stretches
            let t = std::env::var("ZD1DFU_EMU_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
            d.s.borrow().set_read_timeout(Some(Duration::from_secs(t))).ok();
            d.x(b"R").map_err(|e| Error::Usb("emulator USB reset", e))?;
            let r = d.setup(Setup { bm: 0x80, request: 6, value: 0x0100, index: 0 }, 18, &[]).ok();
            let id = r
                .filter(|r| r.len() >= 12)
                .map(|r| (u16::from_le_bytes([r[8], r[9]]), u16::from_le_bytes([r[10], r[11]])));
            Ok(id.map(|id| (d, id)))
        }
    }

    impl Bus for EmuBus {
        type Dev = EmuDev;

        /// The bridge's device if it is vid:pid, opened as a host OS does after the bus reset, as far as a device stack
        /// needs it: the reset, then for our firmware SET_ADDRESS and SET_CONFIGURATION 1 (ASF refuses interface
        /// requests before both). The bootloader takes its requests as they come.
        async fn open(&self, vid: u16, pid: u16, iface: Iface) -> Result<Option<EmuDev>> {
            let Some((mut d, id)) = self.probe()? else { return Ok(None) };
            if id != (vid, pid) {
                return Ok(None);
            }
            if pid == crate::spec::usb::PID {
                for (req, val) in [(5u8, 1u16), (9, 1)] {
                    d.setup(Setup { bm: 0x00, request: req, value: val, index: 0 }, 0, &[])
                        .map_err(|e| Error::Usb("emulator USB enumeration", e))?;
                }
            }
            d.iface = match iface {
                Iface::Number(n) => n,
                Iface::Class(c, s, p) => {
                    let get = |len| d.setup(Setup { bm: 0x80, request: 6, value: 0x0200, index: 0 }, len, &[]);
                    let head = get(9).map_err(|e| Error::Usb("emulator USB configuration descriptor", e))?;
                    let total = head.get(2..4).map_or(9, |b| u16::from_le_bytes([b[0], b[1]]));
                    let cfg = get(total).map_err(|e| Error::Usb("emulator USB configuration descriptor", e))?;
                    match find_class(&cfg, (c, s, p)) {
                        Some(n) => n,
                        None => return Ok(None),
                    }
                }
            };
            Ok(Some(d))
        }

        async fn present(&self, vid: u16, pid: u16) -> Result<bool> {
            Ok(self.probe()?.is_some_and(|(_, id)| id == (vid, pid)))
        }

        /// In the emulator's time (`Z` on a connection of its own: no reset).
        async fn sleep(&self, ms: u32) {
            match self.connect() {
                Ok(d) => d.sleep(ms).await,
                Err(_) => crate::sleep(ms).await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A device that is there but refuses the open `refusals` times, as a just-enumerated one in a browser can.
    struct Refusing {
        refusals: Cell<u32>,
    }
    struct NoDev;
    impl Usb for NoDev {
        fn iface(&self) -> u8 {
            0
        }
        async fn control_in(&self, _: Setup, _: u16, _: u32) -> std::result::Result<Vec<u8>, UsbError> {
            Err(UsbError::Gone)
        }
        async fn control_out(&self, _: Setup, _: &[u8], _: u32) -> std::result::Result<(), UsbError> {
            Err(UsbError::Gone)
        }
        async fn bulk_out(&self, _: u8, _: &[u8], _: u32) -> std::result::Result<usize, UsbError> {
            Err(UsbError::Gone)
        }
    }
    impl Bus for Refusing {
        type Dev = NoDev;
        async fn open(&self, _: u16, _: u16, _: Iface) -> Result<Option<NoDev>> {
            match self.refusals.get() {
                0 => Ok(Some(NoDev)),
                n => {
                    self.refusals.set(n - 1);
                    Err(Error::Device("WebUSB error".into()))
                }
            }
        }
        async fn present(&self, _: u16, _: u16) -> Result<bool> {
            Ok(true)
        }
        async fn sleep(&self, _: u32) {}
    }
    /// The futures here never wait (the sleeps are no-ops): one poll finishes them.
    fn now<F: Future>(f: F) -> F::Output {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::pin::pin!(f).poll(&mut cx) {
            std::task::Poll::Ready(r) => r,
            std::task::Poll::Pending => panic!("waited"),
        }
    }

    #[test]
    fn a_refused_open_is_tried_again_until_the_time_is_up() {
        let open = |refusals, ms| {
            now(wait_open(&Refusing { refusals: Cell::new(refusals) }, 1, 2, Iface::Number(0), ms, &crate::Quiet, "x"))
        };
        assert!(matches!(open(2, 15_000), Ok(Some(_))));
        assert!(matches!(open(1000, 1_000), Err(Error::Device(_))));
    }

    /// The page: a person at it, asked for the device (the browser's chooser holds it meanwhile), counted.
    struct Person {
        asked: Cell<u32>,
    }
    impl crate::Events for Person {
        fn wants_device(&self, _: u16, _: u16, _: &str) {
            self.asked.set(self.asked.get() + 1);
        }
        fn patient(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_person_asked_for_the_device_has_no_deadline() {
        // refused for 10 minutes of 200 ms steps: far past the 15 s
        let bus = |n| Refusing { refusals: Cell::new(n) };
        let open = |n, ev: &dyn crate::Events| now(wait_open(&bus(n), 1, 2, Iface::Number(0), 15_000, ev, "x"));
        let p = Person { asked: Cell::new(0) };
        assert!(matches!(open(3000, &p), Ok(Some(_))));
        assert_eq!(p.asked.get(), 10, "asked at 1.5 s, then once a minute");
        assert!(matches!(open(3000, &crate::Quiet), Err(Error::Device(_))));
    }

    #[test]
    fn find_class_walks_the_descriptors() {
        // a configuration, an audio control interface with a class-specific header, HID, then the vendor interface
        let mut cfg = vec![9, 2, 0, 0, 3, 1, 0, 0xC0, 50];
        cfg.extend([9, 4, 0, 0, 1, 1, 1, 0, 0]);
        cfg.extend([9, 0x24, 1, 0, 1, 0x28, 0, 1, 1]);
        cfg.extend([9, 5, 0x81, 3, 2, 0, 10, 0, 0]);
        cfg.extend([9, 4, 1, 0, 1, 3, 0, 0, 0]);
        cfg.extend([9, 4, 4, 0, 1, 0xFF, 0x5A, 1, 0]);
        cfg.extend([7, 5, 5, 2, 64, 0, 0]);
        assert_eq!(find_class(&cfg, (0xFF, 0x5A, 0x01)), Some(4));
        assert_eq!(find_class(&cfg, (3, 0, 0)), Some(1));
        assert_eq!(find_class(&cfg, (0xFF, 0x5A, 0x02)), None);
        // a zero length ends the walk
        cfg[9] = 0;
        assert_eq!(find_class(&cfg, (0xFF, 0x5A, 0x01)), None);
    }
}
