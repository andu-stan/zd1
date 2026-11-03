//! What the protocol is. Every item cites its evidence: [RE] = read in the stock firmware's code (address), [HW] =
//! seen on the unit, [EMU] = seen only in the emulator, [INF] = inferred. Details and the stock firmware's
//! handling: docs/protocol/spi-registers.md.

/// F = fixed (2 data bytes), V = variable (16-bit little-endian length, then the data). The stock MCU firmware keeps
/// this in a 256-entry table (0x8000a27c) and reads every register its own way: a notification has to be answerable
/// both ways (the network status is notified and then read F-style).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    F,
    V,
}

/// Who does what with a register (bit set).
pub type Access = u8;
/// The MCU reads it.
pub const R: Access = 1;
/// The MCU writes it.
pub const W: Access = 2;
/// The module pushes it (a notification: 0x0C names it, then the MCU reads it).
pub const N: Access = 4;

/// What a register carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payload {
    /// A 16-bit value; b0 (the first wire byte) is the high byte.
    U16,
    /// Two ASCII characters (NetStatus "21").
    Ascii2,
    /// A NUL-terminated ASCII string of at most `max` bytes (with the NUL).
    CStr { max: u16 },
    /// ASCII without a NUL, at most `max` bytes.
    Text { max: u16 },
    /// Raw bytes, at most `max`.
    Bytes { max: u16 },
    /// `03 03 01 vv`: AirPlay volume as attenuation 0..240, more = mute ([`crate::codec::volume`]).
    Volume,
    /// DRC XML: `<update id="status"><icon id="play">pl|pa|em</icon></update>` ([`crate::codec::play`]).
    PlayXml,
    /// The configuration channel: `[op][nameLen][name][value]` ([`crate::codec::config`]).
    Config,
    /// A DRC code (b0, [`Drc`]).
    Drc,
}

pub struct Register {
    pub num: u8,
    pub name: &'static str,
    pub kind: Kind,
    pub access: Access,
    pub payload: Payload,
    pub doc: &'static str,
}

macro_rules! registers {
    ($( $id:ident = $num:literal $kind:ident [$($acc:ident)|+] $payload:expr, $doc:literal; )*) => {
        /// Register numbers.
        pub mod reg { $( #[doc = $doc] pub const $id: u8 = $num; )* }
        /// The registers we know and use, in number order.
        pub const REGISTERS: &[Register] = &[ $( Register {
            num: $num, name: stringify!($id), kind: Kind::$kind, access: 0 $(| $acc)+, payload: $payload, doc: $doc,
        }, )* ];
    };
}

registers! {
    PENDING = 0x0C F [R] Payload::U16,
        "Pending: b0 = the register with queued data (0 = none). Read while PA20 is high. [RE 0x8000ca5c]";
    STATUS = 0x11 F [R|N] Payload::U16,
        "DM1000Status: 0x2100 ready; 0x2300 BL2 running; 0x27 asks the MCU to reboot into DFU. [RE 0x8000b684]";
    VERSION_IMAGE = 0x25 V [R] Payload::CStr { max: 64 },
        "VersionImage: the stock app's build (\"I2014092908219112\": the stock MCU firmware wants \"9112\" in stripped characters 13..16, else system error 7 [RE 0x80009340]). Ours: \"zd1-os VERSION KIND\" (KIND release, debug or rescue) from our OS, \"zd1-boot\" from our boot stub while its status says booting. Our MCU reads it at ready (and at booting): ours if it starts with \"zd1-\".";
    HCR = 0x28 F [W] Payload::U16,
        "HCR Ctrl: 0x2100 (0x2120 from the MCU bootloader) = reboot into BL2; in BL2, 0x2220 = start an XModem download over 0x2D / 0x2C (the bootloader's relay sends it once BL2 runs) [EMU trace of the bootloader 2026-10-26]; 0x2421 factory reset. [RE]";
    HCR_STATUS = 0x29 F [R|N] Payload::U16, "HCR status: 0x2020 / 0x2120 after HCR, 0x22xx in BL2. [RE 0x8000b60c]";
    XM_MODULE = 0x2C V [R|N] Payload::Bytes { max: 1029 },
        "XModem, module -> MCU: BL2 receiving (an image through 0x2D): its control bytes 'C' / ACK / NAK / CAN, one per notification. [RE stock MCU 0x80008b40; the relay uses it, HW 2026-10-26]";
    XM_HOST = 0x2D V [W] Payload::Bytes { max: 1029 },
        "XModem, MCU -> module: a whole XModem-1K frame (1029 bytes) per V write; a 1-byte write is a control byte (EOT). [RE stock MCU 0x80008ba0, the MCU bootloader's relay]";
    NET_STATUS = 0x32 F [R|N] Payload::Ascii2,
        "NetStatus: both characters != '0' = connected (stock live: \"21\", also on wired). [RE 0x8000b518]";
    IP = 0x38 V [R|N] Payload::CStr { max: 16 }, "IP address string. [RE 0x8000d1b4]";
    NETMASK = 0x39 V [R|N] Payload::CStr { max: 16 }, "Netmask string (notified; not in the MCU dispatch table). [RE]";
    GATEWAY = 0x3A V [R|N] Payload::CStr { max: 16 }, "Gateway string. [RE]";
    VOLUME = 0x40 V [R|W|N] Payload::Volume,
        "Volume both ways (same handler as 0xE4): 03 03 01 vv; also the 03 01 handshake. [RE 0x8000b32c]";
    CONFIG = 0x45 V [W|R|N] Payload::Config,
        "Configuration channel: reads reply by notification on 0x45. Ours: names `zd1/KEY` are our settings, `zd1/@N` our records by number (the host link's `Record`; docs/module/settings.md). [RE 0x8000b458]";
    DRC_XML = 0x47 V [R|N] Payload::PlayXml, "DRC XML: the play icon pl / pa / em; pl makes the MCU switch to Wi-Fi. [RE]";
    DRC = 0x48 F [W] Payload::Drc, "DRC command (b0, [`Drc`]); some acknowledged on 0xE3. [RE]";
    DEVICE_NAME = 0x58 V [R|N] Payload::Text { max: 64 }, "DeviceName (no NUL from the stock module). [RE]";
    ACTIVE_PROFILE = 0x5A F [R|W|N] Payload::Ascii2, "ActiveWirelessProfile: b0 = '0' + n. [RE]";
    INTENT = 0xC0 F [R|N] Payload::U16,
        "Ours: a person picked one of our OS's receivers (the speaker chosen on a phone for AirPlay: the sender's first SETUP; in a Spotify app): b0 = the [`player`] receiver, b1 counts up (each pick is a new value). zd1-linkd has made it the receiver in front (the others pause); the MCU switches to the module at once, as a choice made by hand, acknowledges it on the LED and starts the amp.";
    DEBUG = 0xC1 F [W] Payload::U16,
        "Ours: the module OS's debug services: b0 = 0 the image's default (zd1.debug), 1 on, 2 off (host::Debug). The MCU writes it at every ready of our OS once set, and when it changes.";
    SELECT = 0xC2 F [W] Payload::U16,
        "Ours: b0 = a [`player`] receiver to bring to the front, as a person chose it on the speaker (the ON/OFF key, the host link's Select): zd1-linkd tells the others to pause their senders and that one to resume.";
    MCU_VERSION = 0xC3 V [W] Payload::CStr { max: 48 },
        "Ours: the MCU firmware's version, written at every ready of our OS (the module keeps it in /run/zd1/mcu-version).";
    BOOT = 0xC4 F [R|W] Payload::U16,
        "Ours: what the boot stub starts (module/boot linuxboot, identified as \"zd1-boot\" on 0x25 while its status says booting): b0 = a [`boot`] value, b1 = the good slot. The MCU writes it once per boot of the stub (it reads 0 until then); without an answer the stub starts the rescue. The MCU writes it to our OS at ready too: what it runs and the good slot (a trial confirmed by that ready).";
    UPDATE = 0xC5 V [W] Payload::Bytes { max: 128 },
        "Ours: start the updater (zd1-update, docs/module/updater.md): size u32, baud u32 (little-endian), sha256 [32], the stamp (ASCII, the rest). The system image follows on the module console's UART at that baud, in frames the module answers (docs/module/updater.md); its lines (0x02 text \\n) say ready, ok or error.";
    PLAYER = 0xC6 V [R|N] Payload::Bytes { max: 3 },
        "Ours: our OS's receivers: [0] the one in front (a [`player`] receiver: the one playing, else the last one paused or chosen), [1] its [`player`] state, [2] the receivers with a sender connected (bit = the receiver's value: an AirPlay sender, a Spotify Connect session, a snapserver). Notified by zd1-linkd when any of it changes (its arbitration: the newest `playing` source owns the speaker) and once our MCU firmware has written 0xC3; never before (a stock host can't read it and its queue would stay stuck on it). The MCU's source list, LED and auto-switch use it: a receiver's playing switches the source and wakes the speaker (zd1-linkd sends no 0x47).";
    SPEAKER = 0xD2 V [W] Payload::Bytes { max: 4 },
        "Ours, native ([`LINK_PROTO`]): the speaker, as our OS should see it: [0] [`native::PROTO`], [1] the module's volume, a step (0..71: the speaker's, or one the module asked for that waits for it to be the input), [2] the highest step the module may ask for (the cap; a sender's whole range maps onto 0..cap), [3] [`native`] flags (the module is the input). Written after 0xC3 at every hello and whenever any of it changes, the volume also after every 0xD4 (what was applied: the cap, the knob).";
    KEY = 0xD3 F [W] Payload::U16,
        "Ours, native: a key a person pressed for the module (the speaker's keys, the remote, the host link's Key, a USB host's media keys while the module is the input): b0 a [`native`] key, b1 counts up. Only presses: entering and leaving the module's input are 0xD2's flag, a receiver chosen on the speaker is 0xC2.";
    VOLUME_SET = 0xD4 F [R|N] Payload::U16,
        "Ours, native: our OS asks for a volume: b0 a step 0..the cap (0xD2 [2]). Applied at once while the module is the input, else when it becomes it (a later knob change wins); the result comes back on 0xD2.";
    LINK_PROTO = 0xD5 F [R] Payload::U16,
        "Ours: the protocol our OS speaks, b1 ([`native::PROTO`]). The MCU reads it at every hello; another value than its own makes the module no source (the base layer still works).";
    STATE = 0xD6 V [N] Payload::Bytes { max: 1024 },
        "Ours, native: the State record (the host link's `State` message: the module's settings and status, encoded by the module with the host link's codec). Notified after every hello and when something in it changes (at most once a second); the MCU keeps the last copy and serves it to the host (`ModRecord State`) without asking the module.";
    SHUTDOWN = 0xE2 F [W|N] Payload::U16, "Shutdown: the MCU writes 00 00, the module answers 01 00, then power off. [RE]";
    DRC_ACK = 0xE3 F [R|N] Payload::U16, "AirTunesPlaybackStatus: the DRC acks (b0). [RE]";
    VOLUME_HOST = 0xE4 V [R|N] Payload::Volume, "VolumeHostChange: as 0x40. [RE]";
}

/// The stock MCU firmware's register kind table (0x8000a27c; re/mcu/work/bco_link_notes.txt). [RE]
pub const STOCK_F: &[u8] = &[
    0x05, 0x06, 0x07, 0x08, 0x0c, 0x11, 0x28, 0x29, 0x2a, 0x2b, 0x30, 0x31, 0x32, 0x34, 0x35, 0x36, 0x37, 0x3b, 0x46,
    0x48, 0x49, 0x4a, 0x4d, 0x50, 0x51, 0x52, 0x53, 0x54, 0x56, 0x57, 0x5a, 0x5b, 0x60, 0x6a, 0x6b, 0x74, 0x76, 0x7a,
    0xc7, 0xc9, 0xca, 0xcc, 0xce, 0xe2, 0xe3,
];
pub const STOCK_V: &[u8] = &[
    0x00, 0x25, 0x26, 0x27, 0x2c, 0x2d, 0x2e, 0x2f, 0x33, 0x38, 0x39, 0x3a, 0x3c, 0x3d, 0x3e, 0x3f, 0x40, 0x45, 0x47,
    0x4b, 0x4c, 0x4e, 0x4f, 0x55, 0x58, 0x59, 0x5c, 0x5d, 0x5e, 0x5f, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x68, 0x69,
    0x6c, 0x75, 0x77, 0x78, 0x79, 0x7b, 0xc8, 0xcb, 0xcd, 0xcf, 0xd0, 0xd1, 0xdc, 0xdd, 0xe4,
];

/// A register's kind: ours, else the stock table's, else F.
pub const fn kind(num: u8) -> Kind {
    let mut i = 0;
    while i < REGISTERS.len() {
        if REGISTERS[i].num == num {
            return REGISTERS[i].kind;
        }
        i += 1;
    }
    let mut i = 0;
    while i < STOCK_V.len() {
        if STOCK_V[i] == num {
            return Kind::V;
        }
        i += 1;
    }
    Kind::F
}

pub fn register(num: u8) -> Option<&'static Register> {
    REGISTERS.iter().find(|r| r.num == num)
}

macro_rules! codes {
    ($(#[doc = $tdoc:literal])* $tname:ident { $( $id:ident = $code:literal ack $ack:tt, $doc:literal; )* }) => {
        $(#[doc = $tdoc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        pub enum $tname { $( #[doc = $doc] $id = $code, )* }
        impl $tname {
            pub const ALL: &'static [$tname] = &[ $( $tname::$id, )* ];
            pub const fn code(self) -> u8 { self as u8 }
            pub fn from_code(c: u8) -> Option<$tname> { match c { $( $code => Some($tname::$id), )* _ => None } }
            /// The value the module acknowledges it with on 0xE3 (b0), if any.
            pub const fn ack(self) -> Option<u8> { match self { $( $tname::$id => codes!(@ack $ack), )* } }
            pub const fn name(self) -> &'static str { match self { $( $tname::$id => stringify!($id), )* } }
            pub const fn doc(self) -> &'static str { match self { $( $tname::$id => $doc, )* } }
        }
    };
    (@ack -) => { None };
    (@ack $a:literal) => { Some($a) };
}

codes! {
    /// DRC codes, MCU -> module on 0x48 (b0). [RE: docs/protocol/spi-registers.md 3.6]
    Drc {
        JbcStart = 0x9D ack -, "JBConnect: start the setup network (after Networking/JBCONNECT/JBCSSID).";
        WirelessApply = 0x9E ack -, "Re-apply the wireless settings (sent when NetIF/mode was set to wireless).";
        JbcStop = 0x9F ack -, "JBConnect: stop the setup network.";
        Next = 0xB0 ack -, "Next track (key/IR on the Wi-Fi source).";
        Prev = 0xB1 ack -, "Previous track.";
        PlayPause = 0xB3 ack -, "Play/pause toggle (IR).";
        Pause = 0xDE ack 1, "Pause: leaving the Wi-Fi source (other source, standby, iPod docked).";
        Play = 0xDF ack 0, "Play/resume: entering the Wi-Fi source, and the startup resume.";
        Release = 0xE1 ack 4, "Release the audio path: leaving Wi-Fi for a local source or standby.";
        Enable = 0xE2 ack 5, "(Re)enable the audio path: entering the Wi-Fi source, and at boot.";
    }
}

/// A named value of the protocol, for the table the MCU's C is generated from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Int(u32),
    Str(&'static str),
}
impl From<u8> for Value {
    fn from(v: u8) -> Value {
        Value::Int(v as u32)
    }
}
impl From<u16> for Value {
    fn from(v: u16) -> Value {
        Value::Int(v as u32)
    }
}
impl From<&'static str> for Value {
    fn from(v: &'static str) -> Value {
        Value::Str(v)
    }
}

/// A group of named values (a module here, `ZD1_<GROUP>_<NAME>` in C).
pub struct Group {
    pub name: &'static str,
    pub doc: &'static str,
    pub items: &'static [Const],
}
pub struct Const {
    pub name: &'static str,
    pub value: fn() -> Value,
    pub doc: &'static str,
}

/// Groups of values: each a module of constants (with any functions that belong to them after a `;`), and the table
/// `CONSTS` the MCU's C is made from.
macro_rules! consts {
    ($( $(#[doc = $gdoc:literal])* $group:ident {
        $( $(#[doc = $doc:literal])* $name:ident : $ty:ty = $val:expr, )*
        $(; $($item:item)*)?
    } )*) => {
        $(
            $(#[doc = $gdoc])*
            pub mod $group {
                #[allow(unused_imports)]
                use super::*;
                $( $(#[doc = $doc])* pub const $name: $ty = $val; )*
                $($($item)*)?
            }
        )*
        /// Every group, as the MCU's C gets it.
        pub const CONSTS: &[Group] = &[ $( Group {
            name: stringify!($group),
            doc: concat!($($gdoc),*).trim_ascii_start(),
            items: &[ $( Const { name: stringify!($name), value: || Value::from($group::$name),
                                 doc: concat!($($doc),*).trim_ascii_start() }, )* ],
        }, )* ];
    };
}

consts! {
    /// The wire (framing.rs): every byte stuffed, ESC ESC for 0x27, ESC 01 for 0xFF; a raw 0xFF is idle. A request's
    /// first byte is its type.
    wire {
        ESC: u8 = 0x27,
        IDLE: u8 = 0xFF,
        WRITE_F: u8 = 0x00,
        READ_F: u8 = 0x01,
        WRITE_V: u8 = 0x02,
        READ_V: u8 = 0x03,
        /// The longest V write the stock MCU sends [RE 0x8000b910].
        V_MAX: u16 = 0x405,
    }

    /// DM1000Status values (0x11, the 16-bit value).
    status {
        /// Our boot stub, while it waits for the MCU's choice (0xC4).
        BOOTING: u16 = 0x2000,
        READY: u16 = 0x2100,
        BL2: u16 = 0x2300,
    }

    /// Volume (0x40 / 0xE4, `03 03 01 vv`): the MCU's steps, and the attenuation that mutes (codec.rs).
    volume {
        STEPS: u8 = 71,
        MUTE: u8 = 0xFF,
    }

    /// The play state the stock module sends on 0x47 (DRC XML): the MCU looks for the pattern and compares the two
    /// letters after it [RE].
    play {
        PATTERN: &str = "<icon id=\"play\">",
        ICON_PLAYING: &str = "pl",
        ICON_PAUSED: &str = "pa",
        ICON_EMPTY: &str = "em",
    }

    /// Which receiver has the speaker (reg 0xC6): b0 the receiver, b1 its state.
    player {
        /// None: nothing has played since the module started, or the last owner stopped.
        NONE: u8 = 0,
        AIRPLAY: u8 = 1,
        SPOTIFY: u8 = 2,
        SNAPCAST: u8 = 3,
        /// An app that names no receiver we know (`zd1-ctl state playing`).
        OTHER: u8 = 4,
        /// b1: stopped (no session, or nothing queued).
        STOPPED: u8 = 0,
        /// b1: a sender is connected but paused.
        PAUSED: u8 = 1,
        /// b1: playing.
        PLAYING: u8 = 2,
        ;
        /// The receiver for a source name in zd1-linkd's `state STATE SOURCE`.
        pub fn from_source(name: &str) -> u8 {
            match name { "airplay" => AIRPLAY, "spotify" => SPOTIFY, "snapcast" => SNAPCAST, _ => OTHER }
        }
        /// The source name of a receiver (`app` for OTHER), None for NONE or an unknown value.
        pub fn source_name(p: u8) -> Option<&'static str> {
            match p { AIRPLAY => Some("airplay"), SPOTIFY => Some("spotify"), SNAPCAST => Some("snapcast"), OTHER => Some("app"), _ => None }
        }
    }

    /// What the boot stub starts (reg 0xC4 b0; the host link's `Slot` takes these).
    boot {
        /// System slot A (mtd1) as the root file system.
        SLOT_A: u8 = 1,
        /// System slot B (mtd2).
        SLOT_B: u8 = 2,
        /// The rescue initramfs in the app partition.
        RESCUE: u8 = 3,
        /// What the stub calls itself on 0x25 (the MCU answers only it).
        IDENT: &str = "zd1-boot",
    }

    /// The native protocol between our MCU firmware and our OS (0xD2-0xD5): the speaker's features. The MCU drives
    /// the module as a source only when 0xD5 says this very version; anything else (the stock app, our OS with
    /// another version) gets the base layer alone (status, 0x25, 0xD5, 0xC3, 0xC4, 0xC5, 0xE2, HCR; docs/web.md) and
    /// is a state the host sees. The stock app's own registers (0x40, 0x47, 0x48, 0xE3) are documented, not spoken.
    native {
        /// The version our OS answers on 0xD5 (b1) and the MCU writes in 0xD2 [0]; another one on either side: no source.
        PROTO: u8 = 1,
        /// 0xD2 [3]: the module is the speaker's input (its audio path enabled).
        INPUT: u8 = 1,
        /// Keys (0xD3 b0).
        KEY_PLAY_PAUSE: u8 = 1,
        KEY_NEXT: u8 = 2,
        KEY_PREV: u8 = 3,
        ;
        /// A key's name on zd1-linkd's app socket (`key NAME SOURCE`).
        pub fn key_name(k: u8) -> Option<&'static str> {
            match k { KEY_PLAY_PAUSE => Some("playpause"), KEY_NEXT => Some("next"), KEY_PREV => Some("prev"), _ => None }
        }
        /// A step from a share of the module's range (0-100 % of 0..cap), and back: exact both ways for every step
        /// while the cap is under 100.
        pub fn step_from_percent(p: u8, cap: u8) -> u8 { ((p.min(100) as u16 * cap as u16 + 50) / 100) as u8 }
        pub fn percent_from_step(step: u8, cap: u8) -> u8 {
            if cap == 0 { 0 } else { ((step.min(cap) as u16 * 100 + cap as u16 / 2) / cap as u16) as u8 }
        }
    }
}
