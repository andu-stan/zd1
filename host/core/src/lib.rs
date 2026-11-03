//! zd1-host: what a computer does with the Zeppelin Air over USB, written once. The command line (host/cli, natively)
//! and the web page (host/web, WebAssembly over WebUSB) are front-ends to it; neither has a flow of its own.
//!
//! - [`mcu`]: the host link to our MCU firmware (docs/protocol/host.md): typed requests, the MCU's log, the gate.
//! - [`settings`]: the module OS's settings (docs/module/settings.md), the Wi-Fi key derived here.
//! - [`speaker`]: the play choices, and the speaker's state in words.
//! - [`update`]: the module's system into the slot that doesn't run, then started once (docs/module/updater.md).
//! - [`dfu`]: the B&W DFU bootloader (03EB:2FF8): read, erase, program, verify, start; the relay to the module.
//! - [`flash`]: our MCU firmware onto the speaker: into DFU, a backup, flash, verify, start, check.
//! - [`install`]: the speaker as a whole: our firmware on both boards from any state, and the way back to stock.
//! - [`image`]: the files: MCU images (Intel HEX, the app region's .bin, their identification), system images,
//!   `.bcd`s, told apart by [`image::identify`].
//! - [`backup`]: the MCU's backup before a flash, one tar with its manifest.
//! - [`lab`]: measurements over the host link: the bulk pipe's test.
//! - [`xmodem`]: a module image through BL2's XModem over SPI (recovery with our MCU firmware running).
//!
//! The transport is [`usb::Usb`]: nusb (macOS and Linux, and WebUSB in the browser; Windows needs WinUSB descriptors the
//! firmware doesn't have yet), or the emulator's USB
//! bridge (EP0 only: `ZD1DFU_EMU`). A flow reports to [`Events`]: the MCU's log, a press wanted on the speaker,
//! progress, a person's lines; it asks [`Events::cancelled`] where stopping is safe.

pub mod backup;
pub mod console;
pub mod dfu;
pub mod flash;
pub mod image;
pub mod install;
pub mod lab;
pub mod mcu;
pub mod settings;
pub mod speaker;
pub mod update;
pub mod usb;
pub mod xmodem;

pub use mcu::Mcu;
pub use zd1_hostlink as spec;
pub use zd1_hostlink::codec::{self, Msg, Val};
pub use zd1_hostlink::msg;

use std::fmt;

/// Why something failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A USB transfer failed: what was being done, and how.
    Usb(&'static str, usb::UsbError),
    /// The MCU answered `Error`: why, and its text.
    Mcu(spec::Err, String),
    /// No answer in time.
    Timeout(String),
    /// A message that doesn't decode (or a request that doesn't encode).
    Codec(codec::Error),
    /// The device isn't on USB, or isn't what was expected.
    Device(String),
    /// A flow's own verdict: a refused image, a failed verify, an update the module rejected.
    Failed(String),
    /// Stopped by the person ([`Events::cancelled`]).
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Usb(what, e) => write!(f, "{what}: {e}"),
            Error::Mcu(code, text) => write!(f, "{}: {text}", code.name().to_uppercase()),
            Error::Timeout(s) | Error::Device(s) | Error::Failed(s) => f.write_str(s),
            Error::Codec(e) => write!(f, "{e}"),
            Error::Cancelled => f.write_str("stopped"),
        }
    }
}

impl Error {
    /// What kind of failure, for programs (the page, zd1's exit status): `GONE` (the device left USB), `TIMEOUT`,
    /// `LOCKED` (the gate), `DENIED` (nobody pressed), `CANCELLED`, the MCU's other codes by name (`ARG`, `BUSY`,
    /// `MODULE`, ...), `DEVICE` (not on USB, or not what was expected), else `FAILED`.
    pub fn code(&self) -> &'static str {
        use spec::Err as E;
        match self {
            Error::Usb(_, usb::UsbError::Gone) => "GONE",
            Error::Usb(_, usb::UsbError::Timeout) | Error::Timeout(_) => "TIMEOUT",
            Error::Usb(..) | Error::Device(_) => "DEVICE",
            Error::Mcu(e, _) => match e {
                E::Locked => "LOCKED",
                E::Denied => "DENIED",
                E::Unknown => "UNKNOWN",
                E::Malformed => "MALFORMED",
                E::Busy => "BUSY",
                E::Module => "MODULE",
                E::Arg => "ARG",
                E::Unlisted(_) => "MCU",
            },
            Error::Cancelled => "CANCELLED",
            Error::Codec(_) | Error::Failed(_) => "FAILED",
        }
    }
}

impl std::error::Error for Error {}

impl From<codec::Error> for Error {
    fn from(e: codec::Error) -> Error {
        Error::Codec(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a flow tells the person in front of it, and asks them. Every method has a default: a front-end implements
/// what it shows.
pub trait Events {
    /// The MCU's log text (its prints, the module's console among them), as it comes.
    fn text(&self, _b: &[u8]) {}
    /// The speaker wants a press of ON/OFF (the gate, docs/protocol/host.md): say so.
    fn prompt(&self, _s: &str) {}
    /// A line for the person: what a flow does now, or found.
    fn step(&self, _s: &str) {}
    /// A flow of several phases enters phase `n` of `of`, called `what` (the install: the relay, the firmware, ...).
    fn phase(&self, _n: usize, _of: usize, _what: &str) {}
    /// How far a long transfer is (`what`: "update", "read", "write", "verify", "relay", ...).
    fn progress(&self, _what: &str, _done: u64, _total: u64) {}
    /// The person wants it stopped: flows look at this where stopping is safe.
    fn cancelled(&self) -> bool {
        false
    }
    /// A device the program may not use yet (WebUSB: the person grants it in the browser's chooser): ask for it.
    fn wants_device(&self, _vid: u16, _pid: u16, _what: &str) {}
}

/// No one to tell.
pub struct Quiet;
impl Events for Quiet {}

/// The time since some start, in ms (for timeouts and rates).
pub fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::OnceLock;
        static T0: OnceLock<std::time::Instant> = OnceLock::new();
        T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
    }
}

/// Wait `ms` (wall-clock; the emulator's waits go through [`usb::Usb::sleep`]).
pub async fn sleep(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let p = js_sys::Promise::new(&mut |res, _| {
            let g = js_sys::global();
            let st = js_sys::Reflect::get(&g, &"setTimeout".into()).unwrap().unchecked_into::<js_sys::Function>();
            let _ = st.call2(&g, &res, &(ms as f64).into());
        });
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

/// A number that differs from run to run (the clock's sub-second part; the browser's random).
pub fn seed() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Math::random() * 4294967296.0) as u32
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() ^ std::process::id())
    }
}

/// Now, UTC, as `2026-11-01T19:30:00Z`.
pub fn utc_now() -> String {
    #[cfg(target_arch = "wasm32")]
    let secs = (js_sys::Date::now() / 1000.0) as i64;
    #[cfg(not(target_arch = "wasm32"))]
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    // days to the civil date (Howard Hinnant's days_from_civil, inverted)
    let (days, s) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s / 60 % 60, s % 60)
}

/// Hex, lower case.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256.
pub fn sha256(b: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(b).into()
}
