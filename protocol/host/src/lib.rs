//! The host link: how a computer on the speaker's USB port talks to our MCU firmware. Typed messages, the one
//! definition both sides are built from: the computer's side uses this crate's types and codec (`host/core`), the
//! MCU's C (`mcu/src/zd1host_gen.h`) is generated from its table (`protocol/gen`), with the reference
//! (`docs/protocol/host.md`) and test vectors the C is checked against. The spec below is two declarations,
//! `enums!` and `messages!` (macros.rs): each makes the Rust types and the table at once. The MCU's other link, the
//! SPI register protocol to the network module, is `zd1-spi` (protocol/spi).
//!
//! Transport: control transfers on EP0 to a vendor interface ([`usb`]): transfers are checked and retried by USB itself,
//! a message is one transfer (no framing to resync), and the emulator's USB bridge (EP0 only) carries them unchanged.
//! The module's console goes over it too (`Console`, below): the USB device has no serial port.
//!
//! Bulk data (images for the network module) goes to the vendor interface's one endpoint, bulk OUT [`usb::BULK_OUT`]
//! (64-byte packets, two banks; below audio's endpoint, so its alternate-setting switches never re-allocate the pipe);
//! the
//! `Pipe` request says where it goes. The MCU NAKs while its two 512-byte buffers are full: nothing is dropped. The
//! MCU hands data on in 512-byte buffers, or at a short packet: a transfer whose length is a multiple of 64 but not of
//! 512 ends with a zero-length packet. `Pipe` with `reset` drops a partial buffer and starts clean.
//!
//! A call: CALL (OUT, `wValue` = a sequence number) carries one request message; the MCU runs it from its main loop
//! and queues the reply. POLL (IN) returns records: the MCU's log text (everything it prints: progress of a long
//! request, the module's console (its UART, a line at a time), events) and replies tagged with their sequence number,
//! a reply only after the text that was printed before it. The host polls until its reply is there. CALL stalls while an
//! earlier request is still running. HELLO (IN) answers from the interrupt, so it works while the main loop is busy.
//!
//! The module's console (its UART: BL1's and BL2's menus, our OS's shell): `Console` opens it for this host (gated: it
//! is a root shell on the module), and from then on POLL carries its bytes raw and in order as CONSOLE records, instead
//! of the module's lines in the log's text; `ConsoleWrite` puts bytes into the MCU's 1 KB send ring, as many as fit
//! (the UART's 11.5 KB/s paces it: the host sends the rest again). The console closes when the host stops polling for
//! 2 s, and at a HELLO (a new host); opening starts it afresh (what came before is in the log).
//!
//! A message: its id (u8), then its fields in order, little-endian: u8, u16, u32, bool (0 / 1), str (u8 length +
//! bytes, at most `max`), bytes (u16 length + bytes, at most `max`). Any request can be answered with `Error`
//! instead of its reply.
//!
//! Compatibility: firmware and tools of different ages meet (a hosted page talks to whatever firmware a unit has),
//! so the protocol only grows. An id is never reused; a message gains fields only at its end; a field never changes
//! its type. A decoder takes the fields it knows and ignores bytes after them (a newer peer's fields), and a message
//! that ends at a field boundary before them leaves the rest at zero / false / empty (an older peer's). Only a field
//! cut in the middle, or a length over its maximum, is an error. `HelloReply.requests` says which requests the
//! firmware has; one it doesn't know gets `Error UNKNOWN`. `protocol/host/spec.lock` records every message and field
//! that has existed: `gen --check` fails on a change that breaks these rules ([`VERSION`]).
//!
//! The gate: a request that changes the speaker (`Message::gated`: flashing, the debug services, settings, memory,
//! the module's power, the source and volume, the bulk pipe, the module's console) needs a press at the speaker. Locked, it is answered `Error
//! LOCKED`; `Unlock` waits for a press of the ON/OFF key (the LED flashes purple), then gated requests run for 10
//! minutes after the last one. Reads (`Hello`, `Info`, `Diag`, `GetSetting`, `ModRecord`, `ModRead`) are never gated. `Lock` opens the gate
//! for good (kept in the MCU's EEPROM: a lab unit driven by tools unattended) or closes it again. Anything on the
//! computer can use the USB port, a web page the user lets at it included: the gate is the speaker's own check.

#![no_std]

extern crate alloc;

#[macro_use]
mod macros;
/// The codec: the host side's (host/core), and the reference the generated C is checked against.
pub mod codec;

use alloc::{string::String, vec::Vec};
use codec::Val;

/// What `HelloReply.proto` says: bumped only by a break the compatibility rules don't allow (spec.lock, `gen --break`).
pub const VERSION: u16 = 3;

/// The USB side.
pub mod usb {
    /// Our MCU firmware's VID:PID (the B&W DFU bootloader is 03EB:2FF8).
    pub const VID: u16 = 0x03EB;
    pub const PID: u16 = 0x2FE0;
    /// The vendor interface (class 0xFF): requests go to it on EP0, the pipe's data to [`BULK_OUT`]. Its number in our
    /// firmware; hosts find it by class, subclass and protocol (its number moves with the interfaces before it).
    pub const IFACE: u8 = 3;
    pub const CLASS: u8 = 0xFF;
    pub const SUBCLASS: u8 = 0x5A;
    pub const PROTOCOL: u8 = 0x01;
    /// bRequest values (vendor type, interface recipient, `wIndex` = the interface's number).
    pub const CALL: u8 = 0x01;
    pub const POLL: u8 = 0x02;
    pub const HELLO: u8 = 0x03;
    /// The bulk OUT endpoint (64-byte packets, two banks): data for the `Pipe`.
    pub const BULK_OUT: u8 = 0x05;
}

/// The largest message (request or reply, id included).
pub const MSG_MAX: usize = 256;
/// The largest POLL transfer.
pub const POLL_MAX: usize = 512;
/// The largest record of the module OS (`ModRecord`): what the MCU keeps and one SPI notification carries.
pub const RECORD_MAX: usize = 1024;

/// POLL records: kind (u8), length (u16), payload.
pub mod record {
    /// Log text (bytes, `\n` line ends).
    pub const TEXT: u8 = 1;
    /// A reply: the request's sequence number (u16), then the message.
    pub const REPLY: u8 = 2;
    /// The module's console, raw bytes in order (while `Console` has it open).
    pub const CONSOLE: u8 = 3;
}

/// A field's type, as the table and the wire have it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    U8,
    U16,
    U32,
    Bool,
    /// A u8 whose values are an [`Enum`]'s.
    Enum(&'static str),
    /// Text (UTF-8 by convention), at most `max` bytes (<= 255).
    Str(u8),
    /// Bytes, at most `max`.
    Bytes(u16),
}

/// A field's Rust type without its maximum (what [`Wire::KIND`] says).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    U8,
    U16,
    U32,
    Bool,
    Enum(&'static str),
    Str,
    Bytes,
}

impl Ty {
    /// A Rust type's kind with the spec's maximum (strings and bytes).
    pub const fn of(k: Kind, max: u16) -> Ty {
        match k {
            Kind::U8 => Ty::U8,
            Kind::U16 => Ty::U16,
            Kind::U32 => Ty::U32,
            Kind::Bool => Ty::Bool,
            Kind::Enum(e) => Ty::Enum(e),
            Kind::Str => Ty::Str(max as u8),
            Kind::Bytes => Ty::Bytes(max),
        }
    }
}

/// A Rust type a field can have: its kind, and its value for the codec.
pub trait Wire: Sized {
    const KIND: Kind;
    fn to_val(&self) -> Val;
    /// From a value of the right kind (the codec's decoder made it from the table).
    fn from_val(v: &Val) -> Self;
}

fn num(v: &Val) -> u32 {
    if let Val::N(n) = v { *n } else { 0 }
}
fn bytes(v: &Val) -> &[u8] {
    if let Val::S(b) = v { b } else { &[] }
}

impl Wire for u8 {
    const KIND: Kind = Kind::U8;
    fn to_val(&self) -> Val {
        Val::N(*self as u32)
    }
    fn from_val(v: &Val) -> u8 {
        num(v) as u8
    }
}
impl Wire for u16 {
    const KIND: Kind = Kind::U16;
    fn to_val(&self) -> Val {
        Val::N(*self as u32)
    }
    fn from_val(v: &Val) -> u16 {
        num(v) as u16
    }
}
impl Wire for u32 {
    const KIND: Kind = Kind::U32;
    fn to_val(&self) -> Val {
        Val::N(*self)
    }
    fn from_val(v: &Val) -> u32 {
        num(v)
    }
}
impl Wire for bool {
    const KIND: Kind = Kind::Bool;
    fn to_val(&self) -> Val {
        Val::B(*self)
    }
    fn from_val(v: &Val) -> bool {
        matches!(v, Val::B(true))
    }
}
impl Wire for String {
    const KIND: Kind = Kind::Str;
    fn to_val(&self) -> Val {
        Val::S(self.as_bytes().to_vec())
    }
    fn from_val(v: &Val) -> String {
        String::from_utf8_lossy(bytes(v)).into_owned()
    }
}
impl Wire for Vec<u8> {
    const KIND: Kind = Kind::Bytes;
    fn to_val(&self) -> Val {
        Val::S(self.clone())
    }
    fn from_val(v: &Val) -> Vec<u8> {
        bytes(v).to_vec()
    }
}

/// The types the spec's fields name (`String[64]`, `Bytes[248]`, an enum), for the messages' structs.
#[doc(hidden)]
pub mod types {
    pub use super::*;
    pub use alloc::string::String;
    pub use core::primitive::{bool, u8, u16, u32};
    pub type Bytes = alloc::vec::Vec<u8>;
}

/// A name no value of the enum has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoSuchName;

impl core::fmt::Display for NoSuchName {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str("no such name")
    }
}

impl core::error::Error for NoSuchName {}

/// What every enum of the spec has (for a command line's choices).
pub trait SpecEnum: Copy + core::fmt::Display + core::str::FromStr<Err = NoSuchName> + Wire + 'static {
    const ALL: &'static [Self];
}

pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    /// A setting: the host may write it (`SetSetting`, the key = the name with dots for underscores).
    pub set: bool,
    /// Which of the fields after it the request changes: bit 0 the first ([`Message::mask_bit`]); the others are
    /// read only.
    pub mask: bool,
    /// How the module checks a written string beyond its length: `ip` (IPv4), `cidr` (ADDRESS/PREFIX), `host` (HOST
    /// or HOST:PORT), `pin` (4 digits), `bitrate` (96, 160 or 320), `ssid` (1 to 32 bytes), `network` (`wpa2 KEY
    /// SSID` or `open SSID`, KEY 64 hex digits: a secret, never logged), `reset` (`settings` or `all`); "" none.
    pub format: &'static str,
    /// A setting's value when none is stored, in its string form ("" none).
    pub default: &'static str,
    pub doc: &'static str,
}
impl Field {
    /// A setting's key, as the configuration channel and the settings file have it: `wifi_ssid` -> `wifi.ssid`.
    pub fn key(&self) -> alloc::string::String {
        self.name.replace('_', ".")
    }
}
pub struct Message {
    pub id: u8,
    pub name: &'static str,
    pub fields: &'static [Field],
    /// A request's reply (requests only).
    pub reply: Option<&'static str>,
    /// A request that needs the gate open (module doc).
    pub gated: bool,
    /// How long the host waits for the reply, ms; with `wait`, plus that field's seconds (its value when it is 0).
    pub timeout_ms: u32,
    /// A field of the request that says how long the MCU waits, s, and what 0 means.
    pub wait: Option<(&'static str, u32)>,
    /// Only the emulator's build has this request.
    pub emu: bool,
    /// A record of the module OS (`ModRecord`): encoded by the module, decoded by the host; the MCU relays its bytes.
    pub module: bool,
    pub doc: &'static str,
}

/// The settings: the State record's writable fields.
pub fn settings() -> impl Iterator<Item = &'static Field> {
    message("State").map(|m| m.fields.iter()).into_iter().flatten().filter(|f| f.set)
}
/// A setting by its key (`wifi.mode`).
pub fn setting(key: &str) -> Option<&'static Field> {
    settings().find(|f| f.key() == key)
}
/// The module OS's actions: the `Actions` message's fields (written like settings, no state of their own).
pub fn actions() -> impl Iterator<Item = &'static Field> {
    message("Actions").map(|m| m.fields.iter()).into_iter().flatten()
}
/// An action by its key (`wifi.add`).
pub fn action(key: &str) -> Option<&'static Field> {
    actions().find(|f| f.key() == key)
}
/// An enum: its values (name, value, doc) as the spec declares them (names as the Rust variants: `Usb`).
pub struct Enum {
    pub name: &'static str,
    pub values: &'static [(&'static str, u8, &'static str)],
    pub doc: &'static str,
}

// ---- the spec ------------------------------------------------------------------------------------------------------
// Values shared with the SPI protocol (the boot stub's slot, the receiver with the speaker) are the SPI spec's.

use zd1_spi::spec::{boot, player};

enums! {
    /// Why a request failed (`Error.code`).
    Err {
        /// not a request this firmware knows
        Unknown = 1,
        /// the fields don't decode
        Malformed = 2,
        /// another request is running
        Busy = 3,
        /// the network module is not up or did not answer
        Module = 4,
        /// a value out of range
        Arg = 5,
        /// a gated request with the gate closed: `Unlock` first
        Locked = 6,
        /// `Unlock` was not confirmed on the speaker in time
        Denied = 7,
    }
    /// The speaker's state (mcu/src/source.h).
    SysState {
        /// starting
        Boot = 0,
        /// standby: do not disturb (only a person's key, pick, plug or play wakes it)
        Standby = 1,
        /// playing a source
        Run = 2,
        /// a protection tripped: amp off until cleared
        Error = 3,
    }
    /// The selected source.
    Source {
        /// none
        None = 0,
        /// USB audio
        Usb = 1,
        /// AUX in
        Aux = 2,
        /// the network module
        Wifi = 3,
        /// the dock
        Ipod = 4,
    }
    /// The network module, as the MCU sees it.
    /// How an interface gets its address (a setting per interface).
    NetMode {
        /// the interface stays down
        Off = 0,
        Dhcp = 1,
        /// the address, gateway and name server from the settings
        Static = 2,
    }
    /// The Wi-Fi, as the module's supplicant reports it.
    WifiState {
        /// the radio is off (no supplicant: `wifi.mode` off, or no radio)
        Off = 0,
        /// looking for the network
        Scanning = 1,
        /// associating, the handshake
        Connecting = 2,
        /// associated with `bssid`
        Connected = 3,
        /// the handshake failed: the key is wrong (shown through the supplicant's retries)
        WrongKey = 4,
        /// the network wasn't seen (until it is)
        NotFound = 5,
        /// the access point refused, or the attempt failed another way
        Failed = 6,
        /// on, no network configured: the radio scans when asked (`Scan`)
        Idle = 7,
    }
    /// What a Wi-Fi network's beacons offer, as far as this speaker can join it (`ScanEntry.security`).
    WifiSecurity {
        /// no key
        Open = 0,
        /// WPA or WPA2 personal (a passphrase; WPA3 transition networks too): joins
        Psk = 1,
        /// WPA3 only (SAE): can't be joined (the driver's Wireless Extensions have no SAE)
        Sae = 2,
        /// WEP, or enterprise (802.1X): can't be joined
        Other = 3,
    }
    /// A record of the module OS (`ModRecord`): `State` is pushed by the module and kept by the MCU; others are made
    /// when asked. A list record is entries one after another, each a u16 length (little-endian) and a message.
    Record {
        State = 0,
        /// the Wi-Fi networks around the speaker: a list of `ScanEntry`, the strongest first, one per SSID; the
        /// module answers from its last scan and starts another (read again a few seconds later for it)
        Scan = 1,
        /// the Wi-Fi networks the speaker knows, the one it prefers first: a list of `KnownNetwork`; it joins the
        /// first of them it sees (`Actions`: `wifi.add`, `wifi.first`, `wifi.forget`)
        Networks = 2,
        /// the snapservers on the speaker's networks: a list of `Snapserver`; the module answers from its last look
        /// over mDNS and looks again (read again two seconds later for it)
        Servers = 3,
        /// the controllers paired with the AirPlay receiver: a list of `Pairing`, the oldest first
        Pairings = 4,
    }
    ModState {
        /// unpowered
        Off = 0,
        /// powered, held in reset
        Reset = 1,
        /// powered, waiting for its status to say ready
        Booting = 2,
        /// ready (status 0x2100): our OS, speaking this firmware's link protocol
        Ready = 3,
        /// running, the link attached, the status not checked
        Up = 4,
        /// ready, but running the stock module software: no source (install ours, or restore the stock MCU firmware)
        Stock = 5,
        /// ready, our OS, but its link protocol isn't this firmware's: the base layer only (update one of them)
        Mismatch = 6,
        /// BL2 (status 0x2300)
        Bl2 = 7,
    }
    /// What `Module` does.
    ModOp {
        /// power off (reset, then the supply)
        Off = 0,
        /// the boot-time bring-up, not waiting (off again after 180 s without ready)
        Start = 1,
        /// power up and wait for ready (`timeout_s`), off again if it doesn't come
        Power = 2,
        /// power-cycle, HCR 0x21 at ready, wait for BL2 (status 0x2300)
        Bl2 = 3,
        /// power-cycle and release the reset, nothing else: no SPI, no giving up (a JTAG session). State BOOTING
        Hold = 4,
        /// off, then the bring-up (not waiting)
        Restart = 5,
        /// into BL2 through BL1's serial menu, whatever the module's image: power-cycle, then 0x02 on the console from
        /// the reset's release every 20 ms until BL1's menu is in, `4` CR once (nothing sent blind: the menu has "6)
        /// Erase Flash"); ok once BL1 says "Starting Bootloader Level 2" (BL2's web updater on 169.254.1.1 ~11 s
        /// later), not ok without the menu within 3 s. State BOOTING, as HOLD
        Esc = 6,
    }
    /// The module OS's debug services (telnet shell, memory server).
    Debug {
        /// what the image says (`zd1.debug` on its command line: the debug image)
        Default = 0,
        /// on, until the MCU powers off or is told otherwise
        On = 1,
        /// off, the debug image included
        Off = 2,
    }
    /// What the module's boot stub starts (SPI reg 0xC4).
    Slot {
        /// none
        None = 0,
        /// system slot A (NAND 32-64 MB)
        A = boot::SLOT_A,
        /// system slot B (NAND 64-96 MB)
        B = boot::SLOT_B,
        /// the rescue in the app partition: the MCU link and a shell, takes a system
        Rescue = boot::RESCUE,
    }
    /// What `Boot` does.
    BootOp {
        /// start `slot` once, at the module's next boot: it becomes the good slot when the module gets ready with
        /// it, else the next boot is the good one again
        Try = 1,
        /// make `slot` (A or B) the good one
        Good = 2,
        /// forget a try and the failures since the last ready
        Clear = 3,
    }
    /// Where the bulk OUT endpoint's data goes (`Pipe`).
    Sink {
        /// nowhere: counted and CRC-32'd (measuring the USB path)
        Discard = 0,
        /// to the network module's updater over its console UART, as it grants it (`Update` sets it)
        Module = 1,
    }
    /// What `Update` does.
    UpdateOp {
        /// start the module's updater: it takes `size` bytes from the bulk pipe into the slot that doesn't run
        Begin = 1,
        /// stop sending; the module gives up on its own (10 s without data)
        Abort = 2,
    }
    /// Where an update is (`UpdateReply`).
    UpdateState {
        /// none since the MCU started
        Idle = 0,
        /// the module's updater was asked to start: waiting for its ready
        Starting = 1,
        /// the pipe's data goes to the module as it grants it
        Streaming = 2,
        /// all sent: the module checks it and stamps the slot
        Finishing = 3,
        /// installed: `slot` holds it (start it with `Boot` TRY)
        Done = 4,
        /// `text` says why; the slot holds no system
        Failed = 5,
    }
    /// A key on the speaker or the remote (`Key`).
    Key {
        /// ON/OFF, a short press: wakes from standby; in RUN, the next source (switched 1 s after the last press)
        Onoff = 0,
        /// ON/OFF held: standby
        Hold = 1,
        /// the remote's standby: toggles
        Standby = 2,
        /// volume up a step
        Volup = 3,
        /// volume down a step
        Voldn = 4,
        /// play / pause (to the USB host or AirPlay)
        Play = 5,
        /// next track
        Next = 6,
        /// previous track
        Prev = 7,
    }
    /// The network module's play state, as its DRC XML says.
    Play {
        /// nothing heard yet
        Unknown = 0,
        /// nothing queued
        Empty = 1,
        /// playing
        Playing = 2,
        /// paused
        Paused = 3,
    }
    /// Which of the module OS's receivers has the speaker (SPI reg 0xC6 b0).
    Player {
        /// none (or not our OS)
        None = player::NONE,
        /// AirPlay
        Airplay = player::AIRPLAY,
        /// Spotify Connect
        Spotify = player::SPOTIFY,
        /// Snapcast
        Snapcast = player::SNAPCAST,
        /// another app
        Other = player::OTHER,
    }
    /// What that receiver does (SPI reg 0xC6 b1).
    PlayerState {
        /// no session
        Stopped = player::STOPPED,
        /// a sender connected, paused
        Paused = player::PAUSED,
        /// playing
        Playing = player::PLAYING,
    }
    /// The DSP's input.
    DspIn {
        /// none
        None = 0,
        /// I2S from the MCU (USB audio)
        Usb = 1,
        /// I2S from the network module
        Module = 2,
        /// the codec's ADC (analog AUX)
        Aux = 3,
        /// S/PDIF (optical AUX)
        Optical = 4,
    }
    /// The gate in front of the requests that change the speaker.
    Gate {
        /// gated requests need `Unlock` (a press of ON/OFF on the speaker) first
        Locked = 0,
        /// confirmed on the speaker: open until 10 minutes after the last gated request
        Unlocked = 1,
        /// open for good (`Lock` with open, kept in the EEPROM): for a unit tools drive unattended
        Open = 2,
    }
}

messages! {
    /// Done; nothing to return.
    0x7E Ok {}
    /// The request failed (any request).
    0x7F Error {
        /// why
        code: Err,
        /// for a person
        text: String[64],
    }
    /// Bytes read.
    0x7D Data {
        /// what was read
        data: Bytes[248],
    }
    /// Which firmware this is. Also answered from the interrupt as `usb::HELLO` (no CALL needed).
    0x01 Hello -> HelloReply [free, 1000 ms] {}
    0x41 HelloReply {
        /// the protocol version (`VERSION`): another one is a break the tools refuse
        proto: u16,
        /// the firmware's version: the commit (`-dirty` with uncommitted changes)
        fw: String[40],
        /// the commit's date, UTC
        built: String[24],
        /// `zd1`, or `zd1-emu` for the emulator's build
        board: String[16],
        /// since the MCU started
        uptime_ms: u32,
        /// the requests this firmware has: bit (id % 8) of byte (id / 8)
        requests: Bytes[32],
    }
    /// The speaker's state at a glance: the MCU, the source and amp, the network module.
    0x02 Info -> InfoReply [free, 2000 ms] {}
    0x42 InfoReply {
        /// as `HelloReply.fw`
        fw: String[40],
        /// as `HelloReply.built`
        built: String[24],
        /// since the MCU started
        uptime_ms: u32,
        /// the reset cause at boot (PM RCAUSE: 1 power-on, 2 brown-out, 4 external, 8 watchdog)
        rcause: u8,
        state: SysState,
        source: Source,
        /// 0 (mute) .. 71
        volume: u8,
        /// 0 .. 4
        bass: u8,
        /// the amp interlock: false = the amp stays off (every boot starts allowed unless `Amp` deny with keep
        /// locked it)
        amp_allowed: bool,
        /// the amp supply is on
        amp_on: bool,
        /// what the MCU tells the module OS about its debug services
        debug: Debug,
        mod_state: ModState,
        /// its last status (reg 0x11)
        mod_status: u16,
        /// the module OS's identification (reg 0x25: "zd1-os VERSION KIND"), empty for the stock app
        mod_os: String[64],
        /// its IP address (reg 0x38), empty without one
        mod_ip: String[16],
        /// its device name (reg 0x58)
        mod_name: String[64],
        /// a terminal had the module-console port (CDC) open: always false since the port went (`console_open`)
        modcon_open: bool,
        /// the gate in front of the requests that change the speaker
        gate: Gate,
        /// UNLOCKED: seconds until it locks again
        gate_s: u16,
        /// the amp allowed from every boot (the default; false: `Amp` deny with keep locked it, kept in the EEPROM)
        amp_auto: bool,
        /// the amp is off because the source has been silent (it wakes when sound comes)
        amp_asleep: bool,
        /// the module OS's receiver with the speaker
        player: Player,
        player_state: PlayerState,
        /// a USB stream that starts takes the speaker (`Prefs`)
        auto_switch: bool,
        /// the module OS's receivers with a sender connected: bit (a `Player` value)
        players_on: u8,
        /// the link protocol the module OS answered (reg 0xD5; 0: none, or not our OS)
        mod_proto: u8,
        /// the one this firmware speaks (`native::PROTO`): another on the module is `ModState.Mismatch`
        link_proto: u8,
        /// a source silent this long switches the amp off, s (`Prefs`)
        amp_idle_s: u16,
        /// the highest volume step on every source (`Prefs`)
        volume_cap: u8,
        /// the module's console is open over the host link (`Console`)
        console_open: bool,
    }
    /// Open the gate: the MCU waits up to 30 s for a press of ON/OFF on the speaker (its LED flashes purple; the
    /// press does nothing else), then gated requests run until 10 minutes after the last one. `Error DENIED` if
    /// nobody pressed. Answered at once when the gate is open.
    0x0E Unlock -> Ok [free, 35000 ms] {}
    /// Close the gate now, or open it for good (kept in the EEPROM across power cycles: a lab unit driven by tools
    /// unattended; the user's choice, made at the speaker since this request is gated).
    0x0F Lock -> Ok [gated, 1000 ms] {
        /// true: open for good; false: locked (and no longer open for good)
        open: bool,
    }
    /// Read memory (gated: the MCU's RAM passes settings through).
    0x04 Peek -> Data [gated, 1000 ms] {
        /// address
        addr: u32,
        /// bytes, at most 248
        len: u16,
        /// 32-bit reads (registers): `addr` and `len` multiples of 4; the data big-endian, as in memory
        words: bool,
    }
    /// Write memory.
    0x05 Poke -> Ok [gated, 1000 ms] {
        /// address
        addr: u32,
        /// the bytes
        data: Bytes[240],
        /// 32-bit writes (registers): `addr` and the length multiples of 4, the data big-endian
        words: bool,
    }
    /// Into the B&W DFU bootloader (03EB:2FF8, ~3 s later): the module and amp made safe, ISP_FORCE set, a watchdog
    /// reset. The device leaves USB right after the reply.
    0x06 Dfu -> Ok [gated, 3000 ms] {}
    /// Restart the MCU firmware (watchdog reset; the bootloader starts the app again). The module is powered off
    /// first.
    0x07 Reboot -> Ok [gated, 2000 ms] {}
    /// Power and boot the network module (POWER, BL2: the first boot after a `.bcd` install takes ~46-61 s).
    /// Progress comes as text. POWER, BL2 and HOLD run from the main loop (the speaker goes on meanwhile); the reply
    /// comes when they end (HOLD: at the reset's release), and the next request waits for it.
    0x08 Module -> ModuleReply [gated, 10000 ms + timeout_s s or 120] {
        op: ModOp,
        /// for POWER and BL2: how long to wait (0 = 120)
        timeout_s: u16,
    }
    0x48 ModuleReply {
        /// it got there (OFF, START, RESTART: always)
        ok: bool,
        /// now
        state: ModState,
        /// the module's last status (reg 0x11)
        status: u16,
    }
    /// A record of the module OS (docs/module/settings.md): `State`, the speaker as the module knows it (its settings
    /// and its status), pushed by the module on SPI reg 0xD6 and kept by the MCU, which answers at once; others the
    /// module makes when asked (the configuration channel, name `zd1/@N`). The bytes are a message of this spec
    /// (`State`), read in pieces: `offset` into them, the reply says the whole length. A record the module hasn't
    /// comes back as `!` and why.
    0x1E ModRecord -> RecordReply [free, 4000 ms] {
        /// which
        record: Record,
        /// byte offset into it
        offset: u16,
    }
    0x5E RecordReply {
        /// the record's whole length
        total: u16,
        /// from `offset` on
        data: Bytes[240],
    }
    /// The speaker as the module OS knows it: its settings (the fields marked set: written with `SetSetting`, the key
    /// the field's name with dots for underscores, the value in string form; kept on the data partition) and its
    /// status. The module pushes it when anything changes (once a second at most) and after every hello; the MCU
    /// keeps the last copy (`ModRecord State`). The same record, as JSON, is what an API on the module would serve.
    0x60 State [module] {
        /// the speaker's name: AirPlay, Spotify Connect, Snapcast and the MCU's device name; empty: "Zeppelin Air XXXXXX" from the MAC
        name: String[63] set,
        /// the wired interface: off, DHCP (169.254.1.1 until a lease comes), or the fixed address
        eth_mode: NetMode set = "dhcp",
        /// with static: ADDRESS/PREFIX
        eth_address: String[18] set cidr,
        /// with static: the default gateway
        eth_gateway: String[15] set ip,
        /// with static: the name server
        eth_dns: String[15] set ip,
        /// the Wi-Fi: off, DHCP, or the fixed address (and no network configured is off too)
        wifi_mode: NetMode set = "dhcp",
        /// with static: ADDRESS/PREFIX
        wifi_address: String[18] set cidr,
        /// with static: the default gateway
        wifi_gateway: String[15] set ip,
        /// with static: the name server
        wifi_dns: String[15] set ip,
        /// the Wi-Fi network joined or being joined now (the supplicant's); empty: none (the known ones: `Networks`)
        wifi_ssid: String[32],
        /// "(set)" when that network has a WPA2 key, empty when it is open
        wifi_psk: String[64],
        /// the AirPlay receiver
        airplay: bool set = "on",
        /// the Spotify Connect receiver
        spotify: bool set = "on",
        /// the Snapcast client
        snapcast: bool set = "on",
        /// the snapserver, HOST or HOST:PORT; empty: found over mDNS
        snapcast_server: String[64] set host,
        /// the module OS: "zd1-os VERSION KIND"
        os: String[64],
        /// the system slot it runs (NONE: the rescue, or a boot without the stub)
        slot: Slot,
        /// the good slot, as the MCU said at ready
        good: Slot,
        /// the MCU firmware's version, as it greeted
        mcu: String[40],
        /// the wired link is up (a cable with something at the other end)
        eth_link: bool,
        /// the wired interface's IPv4 address now; empty: none
        eth_ip: String[15],
        /// the Wi-Fi's IPv4 address now; empty: none
        wifi_ip: String[15],
        /// the Wi-Fi's state, from the module's supplicant
        wifi: WifiState,
        /// CONNECTED: the access point
        bssid: String[17],
        /// CONNECTED: its signal, -dBm (57 = -57 dBm; 0: unknown)
        signal: u8,
        /// the module is the speaker's input
        input: bool,
        /// the module's volume, % of the speaker's range
        volume: u8,
        /// the receiver in front
        front: Player,
        front_state: PlayerState,
        /// the receivers with a sender connected: bit (a `Player` value)
        senders: u8,
        /// AirPlay's PIN, 4 digits: senders that pair with it are remembered (`Pairings`); empty: pairing without a
        /// PIN, for each connection (as most AirPlay speakers without HomeKit)
        airplay_pin: String[4] set pin,
        /// Snapcast: how much later this speaker plays than the server says, ms (its own latency, as snapclient's)
        snapcast_latency: u16 set = "0",
        /// Spotify Connect's bitrate, kb/s: 96, 160 or 320
        spotify_bitrate: u16 set bitrate = "320",
        /// what system slot A holds: its stamp (the version installed, as `Update.id`); empty: no system (or not read
        /// yet: the module reads both slots' stamps after it starts and after an update)
        stamp_a: String[64],
        /// the same for slot B
        stamp_b: String[64],
    }
    /// A Wi-Fi network seen by the module's radio (the `Scan` record, one entry per SSID; hidden networks are left
    /// out).
    0x61 ScanEntry [module] {
        ssid: String[32],
        /// its signal, -dBm (57 = -57 dBm)
        signal: u8,
        security: WifiSecurity,
    }
    /// A Wi-Fi network the speaker knows (the `Networks` record, in the order it prefers them). Its key never leaves
    /// the speaker.
    0x62 KnownNetwork [module] {
        ssid: String[32],
        /// OPEN, or PSK: a WPA2 key is stored
        security: WifiSecurity,
    }
    /// A snapserver found over mDNS (the `Servers` record): `snapcast.server` takes `host:port`.
    0x63 Snapserver [module] {
        /// its instance name (snapserver's: usually the host's name)
        name: String[63],
        /// its IPv4 address
        host: String[15],
        /// its stream port (1704 unless it says otherwise)
        port: u16,
    }
    /// A controller paired with the AirPlay receiver (the `Pairings` record).
    0x64 Pairing [module] {
        /// the controller's pairing identifier (a UUID-like string its sender chose)
        id: String[63],
        /// it may add and remove pairings
        admin: bool,
    }
    /// The module OS's actions: writes with no state of their own (`SetSetting` with the key the field's name with
    /// dots for underscores; read back empty, or `!` and why the module refused). Not part of `State`.
    0x65 Actions [module] {
        /// join a network: added (or its key changed) and put first: `wpa2 KEY SSID` (KEY: its WPA2 key, 64 hex
        /// digits, derived from the passphrase on the computer) or `open SSID`
        wifi_add: String[102] set network,
        /// forget a known network: its SSID
        wifi_forget: String[32] set ssid,
        /// put a known network first (the speaker prefers it when it sees both): its SSID
        wifi_first: String[32] set ssid,
        /// forget every controller paired with AirPlay (any value): senders pair again; the accessory stays the same
        airplay_forget: String[8] set,
        /// back to the defaults: `settings` (every setting; the networks and AirPlay's pairings stay) or `all` (those
        /// too: as from the factory, but for the software)
        reset: String[8] set reset,
    }
    /// Read a module register over SPI (F: the two data bytes; V: the payload).
    0x09 ModRead -> Data [free, 2000 ms] {
        /// register
        reg: u8,
        /// a V (variable-length) read
        v: bool,
    }
    /// Write a module register over SPI (F: `data` is the two bytes b0 b1).
    0x0A ModWrite -> Ok [gated, 2000 ms] {
        /// register
        reg: u8,
        /// a V write
        v: bool,
        /// payload
        data: Bytes[128],
    }
    /// A module OS setting (docs/module/settings.md: `name`, `wifi.ssid`, ...; `?` lists the keys), through the
    /// configuration channel (reg 0x45, names `zd1/KEY`). Secrets read back as `(set)` or empty.
    0x0B GetSetting -> SettingReply [free, 6000 ms] {
        /// the setting
        key: String[32],
    }
    /// Change a module OS setting: written, then read back. A refused value comes back as `!` and why. The MCU tries
    /// the write three times and the read-back three times, then waits 3 s for the answer.
    0x10 SetSetting -> SettingReply [gated, 8000 ms] {
        /// the setting
        key: String[32],
        /// the new value (empty: back to the default)
        value: String[128],
    }
    0x4B SettingReply {
        /// what the module has now
        value: String[128],
    }
    /// The amp interlock. Allowed, the amp powers up while a source has sound and goes off after it has been silent
    /// for a while; locked, it stays off. Deny also switches the amp off at once. Without `keep` only until the MCU
    /// restarts (a boot starts allowed unless a deny with `keep` locked it).
    0x0C Amp -> Ok [gated, 1000 ms] {
        allow: bool,
        /// also from every boot on (kept in the EEPROM): allow = the amp comes on by itself (the default), deny =
        /// locked
        keep: bool,
    }
    /// The module OS's debug services (telnet shell, memory server): the MCU keeps the choice in RAM and tells the
    /// module now and at every boot of it (reg 0xC1).
    0x0D Debug -> Ok [gated, 2000 ms] {
        mode: Debug,
    }
    /// A key, as if pressed on the speaker or the remote: what it does depends on the state, as with a person.
    0x11 Key -> Ok [gated, 1000 ms] {
        key: Key,
    }
    /// Play a source now, or `NONE`: standby. The amp follows (if allowed). USB and AUX only while plugged in, a
    /// receiver only with a sender connected, nothing after a protection tripped (state ERROR: only ON/OFF or
    /// standby leaves it): `Error ARG` otherwise.
    0x12 Select -> Ok [gated, 2000 ms] {
        source: Source,
        /// WIFI: the module OS's receiver to bring to the front (its sender resumes, the others pause); NONE:
        /// whichever is in front
        player: Player,
    }
    /// The volume and bass (kept in the EEPROM). While USB or AirPlay is the source, their own volume changes it
    /// again.
    0x13 Volume -> Ok [gated, 1000 ms] {
        /// 0 (mute) .. 71; above the volume cap (`InfoReply.volume_cap`) it is the cap
        volume: u8,
        /// 0 .. 4
        bass: u8,
    }
    /// XModem to BL2 over SPI (registers 0x2D / 0x2C), for measuring and driving BL2's receiver: `data` goes into
    /// the MCU's frame buffer at `offset`; with `send`, the buffer up to the end of `data` goes to 0x2D in one V
    /// write (a 1029-byte XModem-1K frame, or 1 byte: EOT) and the MCU waits up to `wait_ms` for BL2's answer on
    /// 0x2C. The reply has every 0x2C byte since the last request: one with none yet asks again with an empty
    /// `XmFrame`. The module must be up (in BL2: `Module BL2`).
    0x15 XmFrame -> XmReply [gated, 2000 ms] {
        /// where `data` goes in the frame buffer (1029 bytes)
        offset: u16,
        /// the next piece of the frame
        data: Bytes[240],
        /// write the buffer [0, offset + len) to 0x2D now and wait for the answer
        send: bool,
        /// send: how long to wait for an answer, ms (0 = none; at most 500: the MCU's watchdog allows no longer)
        wait_ms: u16,
    }
    0x55 XmReply {
        /// the 0x2C bytes since the last XmFrame, oldest first ('C', ACK 06, NAK 15, CAN 18)
        answers: Bytes[32],
        /// send: the V write on SPI, µs
        wire_us: u32,
        /// send: from the end of the write until BL2 raises the request line (its handling), µs (0: no answer
        /// within wait_ms)
        wait_us: u32,
        /// send: then reading the answer out (the pending register, then 0x2C: ours), µs
        read_us: u32,
    }
    /// The bulk pipe: set where the bulk OUT endpoint's data goes, and read its counters (`reset`: from zero).
    0x16 Pipe -> PipeReply [gated, 1000 ms] {
        sink: Sink,
        /// start clean: a partial buffer dropped, the counters and the CRC zeroed
        reset: bool,
    }
    0x56 PipeReply {
        /// now
        sink: Sink,
        /// received since the reset
        bytes: u32,
        /// of those bytes (zlib's)
        crc32: u32,
        /// USB transfers completed (512-byte buffers, or shorter at a short packet)
        packets: u32,
        /// times both buffers were full: the host was NAKed until one was free
        full: u32,
    }
    /// Which system slot the module starts (the MCU keeps the choice in the EEPROM and tells the boot stub at every
    /// boot, SPI reg 0xC4: `docs/module/storage.md`).
    0x17 BootState -> BootReply [free, 1000 ms] {}
    /// Change which slot the module starts (`BootOp`); `restart`: power-cycle the module now so it does.
    0x18 Boot -> BootReply [gated, 1000 ms] {
        op: BootOp,
        /// TRY: A, B or RESCUE; GOOD: A or B
        slot: Slot,
        /// restart the module now (as `Module RESTART`)
        restart: bool,
    }
    /// Update the network module's system (`docs/module/updater.md`): BEGIN tells its updater over SPI (reg 0xC5)
    /// and points the bulk pipe at the module (sink MODULE, from zero); the image then goes to the bulk endpoint,
    /// `size` bytes, and `UpdateStatus` says how far it is. The module must run our OS (a slot or the rescue).
    0x19 Update -> UpdateReply [gated, 2000 ms] {
        op: UpdateOp,
        /// BEGIN: the image's length (a gzip'd cpio of a slot's root)
        size: u32,
        /// BEGIN: its SHA-256, checked by the module before it stamps the slot
        sha256: Bytes[32],
        /// BEGIN: the stamp the slot gets (what `zd1-ctl status` shows)
        id: String[64],
        /// BEGIN: the console UART's speed while it runs (0 = 1000000)
        baud: u32,
    }
    /// How far the module's update is.
    0x1A UpdateStatus -> UpdateReply [free, 1000 ms] {}
    0x59 UpdateReply {
        state: UpdateState,
        /// bytes the module has: in the frames it answered
        sent: u32,
        /// bytes put on the line, resends included
        on_wire: u32,
        /// the image's length
        size: u32,
        /// the slot it goes into (once the module said)
        slot: Slot,
        /// the module's last word: its ready, ok or error line; or why the MCU gave up
        text: String[96],
    }
    /// Open the module's console for this host (POLL's CONSOLE records: the module doc), starting afresh, or close it.
    /// The console's line is the updater's while an update runs: nothing passes then.
    0x1D Console -> ConsoleReply [gated, 1000 ms] {
        open: bool,
    }
    /// Bytes for the module's console (open: `Console`): as many as the MCU's send ring takes now (`taken`); the host
    /// sends the rest again. `Error ARG` while it isn't open.
    0x1F ConsoleWrite -> ConsoleReply [gated, 1000 ms] {
        data: Bytes[240],
    }
    0x5D ConsoleReply {
        /// open for this host now
        open: bool,
        /// `ConsoleWrite`: the bytes of `data` the send ring took
        taken: u16,
        /// the module's bytes lost since it was last opened (a close's reply: while it was open): the host didn't poll
        /// fast enough
        lost: u32,
    }
    /// Data into the bulk pipe as if it came on the bulk endpoint: the emulator's build only (its USB carries EP0
    /// only). `Error BUSY` while both buffers are full: send it again.
    0x1B PipeData -> Ok [gated, 1000 ms, emu] {
        data: Bytes[240],
    }
    0x57 BootReply {
        /// the good slot: started unless a try or a failure says otherwise
        good: Slot,
        /// the slot to start once at the next boot (NONE: none)
        next: Slot,
        /// what the stub was told at the module's last boot (NONE: no stub asked since the MCU started)
        last: Slot,
        /// that boot hasn't got ready yet
        trial: bool,
        /// the slots that didn't get ready since the last ready: bit 0 A, 1 B, 2 the rescue
        failed: u8,
    }
    /// The speaker's behaviour, kept in the EEPROM: switching to a USB stream by itself, how long a silent source
    /// keeps the amp on, the highest volume on every source. `change` says which fields to set
    /// (0: only read them). The amp's lock is `Amp`.
    0x1C Prefs -> PrefsReply [gated, 1000 ms] {
        /// which of the fields after it to set
        change: u8 mask,
        /// a USB stream that starts takes the speaker from another source and wakes it from standby
        auto_switch: bool,
        /// a source silent this long switches the amp off until it has sound again, s: 60 .. 1800 (the amp runs
        /// hot: no longer), in whole minutes
        amp_idle_s: u16,
        /// the highest volume step (1 .. 71) on every source, whatever sets it: the speaker's keys, the remote, this
        /// link's `Volume`, the computer's volume (USB), the network's senders (their whole range maps onto 0 .. this).
        /// A lower cap takes the volume down to it at once. 40 unless set; the heat cap holds it lower while hot
        volume_cap: u8,
    }
    0x5C PrefsReply {
        auto_switch: bool,
        /// the amp allowed from every boot (the default; false: `Amp` deny with keep)
        amp_auto: bool,
        /// how long a source is silent before the amp goes off
        amp_idle_s: u16,
        /// the highest volume step on every source
        volume_cap: u8,
    }
    /// The firmware's internals for debugging: protections, audio path, USB audio, the module's link, the remote.
    0x14 Diag -> DiagReply [free, 2000 ms] {}
    0x54 DiagReply {
        /// ADC4 (PA25), a second temperature (read like ADC5; runs hotter): a trip below 0x1BC
        adc4: u16,
        /// ADC5 (PA26), temperature: falls as it warms, a trip below 0x1FF
        adc5: u16,
        /// ADC6 (PA27), the amp supply rail (PB09): ~0x010 off, ~0x3D5 up
        adc6: u16,
        /// PB04 (fault) low
        fault: bool,
        /// PB18 (amp fault / clip) low
        amp_fault: bool,
        /// consecutive ADC5 samples under its limit (a trip at 9)
        n_temp: u8,
        /// ... ADC4 (a trip at 9)
        n_adc4: u8,
        /// ... PB04 (a trip at 3)
        n_fault: u8,
        /// ERROR: which protection tripped (1 temperature ADC5, 2 amp fault, 3 temperature ADC4, 4 PB04)
        error: u8,
        /// the amp's mute released (PB16)
        amp_unmuted: bool,
        dsp_running: bool,
        dsp_in: DspIn,
        /// the DSP's S/PDIF receiver
        spdif_locked: bool,
        /// the DSP's volume index (0 = +10 dB .. 70 = -60 dB, 71 = mute)
        dsp_volume: u8,
        /// a plug in AUX
        aux_jack: bool,
        /// a host on USB
        usb_vbus: bool,
        /// what the LED shows (mcu/src/led.h): bits 0-2 the source, 3-5 what it does (led_act_t), 6-7 boot /
        /// standby / error / run (led_kind_t)
        led: u8,
        /// RC6 frames decoded since boot
        ir_frames: u16,
        /// the last one: address (0xD2: B&W)
        ir_addr: u8,
        /// command
        ir_cmd: u8,
        /// toggle bit
        ir_toggle: bool,
        /// when (uptime)
        ir_ms: u32,
        /// the host has the audio interface's streaming alternate setting on
        usb_streaming: bool,
        /// its sample rate, Hz
        usb_rate: u32,
        /// the host's mute
        usb_mute: bool,
        /// the host's volume as a step, capped at 40 (0: not set yet)
        usb_step: u8,
        /// iso packets since boot
        usb_packets: u32,
        /// the host configured the media-key interface
        hid: bool,
        /// the SSC/PDCA path runs
        engine: bool,
        /// it saw a bit clock when it started
        clock_ok: bool,
        /// frames in the ring (target 512)
        fill: u16,
        /// playing from the ring (else silence)
        primed: bool,
        underruns: u32,
        overruns: u32,
        /// frames that arrived with the engine off
        dropped: u32,
        /// frames per second played, the last second
        play_fps: u32,
        /// ... received from the host
        host_fps: u32,
        /// the CS2000's ratio now
        ratio: u32,
        /// ... and for the rate: the trim is ratio / nominal - 1
        nominal: u32,
        /// the network module says it has a network
        net: bool,
        play: Play,
        /// a volume one of the module's receivers asked for, as a step, waiting for the module to be the input (255:
        /// none)
        pending_volume: u8,
        /// the module console's UART: overrun and framing errors since boot
        modcon_overruns: u32,
        /// the amp supply's on-time since boot, seconds
        amp_on_s: u32,
        /// how long it has been on now, seconds (0: off)
        amp_run_s: u32,
        /// the volume cap from the heat limit (ADC5 under its soft limit): a step, 71 = none
        heat_cap: u8,
        /// the USB host sends sound (not silence), whichever the source
        usb_audible: bool,
        /// how long the source has been silent, seconds (0: it plays)
        silent_s: u16,
        /// the DSP's input meter (param 0x031, linear peak, 0x800000 = full scale), the last sample: read while
        /// AUX, or Wi-Fi with the stock app, is the input (0 otherwise)
        level: u32,
        /// that source's learnt noise floor (the same scale): sound is 12 dB over it, within -80..-40 dBFS
        level_floor: u32,
    }
}

/// The requests (their ids), as `HelloReply.requests` carries them: bit (id % 8) of byte (id / 8).
pub fn requests_bitmap() -> [u8; 32] {
    let mut b = [0u8; 32];
    for m in MESSAGES.iter().filter(|m| m.reply.is_some()) {
        b[m.id as usize / 8] |= 1 << (m.id % 8);
    }
    b
}

pub fn message(name: &str) -> Option<&'static Message> {
    MESSAGES.iter().find(|m| m.name == name)
}
pub fn enum_(name: &str) -> Option<&'static Enum> {
    ENUMS.iter().find(|e| e.name == name)
}

impl Ty {
    /// Encoded size bounds (min, max).
    pub fn size(self) -> (usize, usize) {
        match self {
            Ty::U8 | Ty::Bool | Ty::Enum(_) => (1, 1),
            Ty::U16 => (2, 2),
            Ty::U32 => (4, 4),
            Ty::Str(m) => (1, 1 + m as usize),
            Ty::Bytes(m) => (2, 2 + m as usize),
        }
    }
}

impl Message {
    /// The largest encoding of this message.
    pub fn max_size(&self) -> usize {
        1 + self.fields.iter().map(|f| f.ty.size().1).sum::<usize>()
    }

    /// How long the host waits for this request's reply, ms (`vals`: its fields, for `wait`).
    pub fn timeout(&self, vals: &[Val]) -> u32 {
        let Some((field, zero)) = self.wait else { return self.timeout_ms };
        let s = match self.fields.iter().position(|f| f.name == field).and_then(|i| vals.get(i)) {
            Some(Val::N(n)) if *n > 0 => *n,
            _ => zero,
        };
        self.timeout_ms + s * 1000
    }

    /// The bit of the mask field that says `field` changes (None: no such field after a mask).
    pub fn mask_bit(&self, field: &str) -> Option<u8> {
        let m = self.fields.iter().position(|f| f.mask)?;
        let i = self.fields.iter().position(|f| f.name == field).filter(|i| *i > m)?;
        (i - m - 1 < 8).then(|| 1 << (i - m - 1))
    }
}

#[cfg(test)]
mod tests;
