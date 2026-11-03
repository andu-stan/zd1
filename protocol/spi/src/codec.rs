//! What the payloads mean.

/// Volume (0x40 / 0xE4, `03 03 01 vv`): AirPlay attenuation 0 (loudest) .. 240, more = mute. The MCU maps it onto
/// its 0..71 volume steps (stock 0x8001cf6c, mcu/src/wifi.c): step = 71 - (vv + 3) * 70 / 240.
pub mod volume {
    pub use crate::spec::volume::{MUTE, STEPS};
    #[inline]
    pub fn encode(att: u8) -> [u8; 4] {
        [0x03, 0x03, 0x01, att]
    }
    /// The attenuation of a volume message, if it is one (`03 03 xx vv`).
    #[inline]
    pub fn decode(p: &[u8]) -> Option<u8> {
        if p.len() >= 4 && p[0] == 0x03 && p[1] == 0x03 { Some(p[3]) } else { None }
    }
    #[inline]
    pub fn step_from_att(att: u8) -> u8 {
        if att > 0xF0 { 0 } else { STEPS.saturating_sub(((att as u16 + 3) * 70 / 240) as u8) }
    }
    #[inline]
    pub fn att_from_step(step: u8) -> u8 {
        let d = STEPS.saturating_sub(step.min(STEPS)) as u16;
        if d >= STEPS as u16 { MUTE } else { (d * 240 / 70) as u8 }
    }
    /// 0..100 % as apps see it.
    #[inline]
    pub fn percent_from_att(att: u8) -> u8 {
        ((step_from_att(att) as u16 * 100 + STEPS as u16 / 2) / STEPS as u16) as u8
    }
    #[inline]
    pub fn att_from_percent(p: u8) -> u8 {
        att_from_step(((p.min(100) as u16 * STEPS as u16 + 50) / 100) as u8)
    }
    /// The `03 01 x 00|03` handshake the stock module starts; the MCU answers `03 01 01 02|04` [RE, meaning unknown].
    #[inline]
    pub fn handshake_reply(p: &[u8]) -> Option<[u8; 4]> {
        if p.len() >= 4 && p[0] == 0x03 && p[1] == 0x01 && (p[3] == 0x00 || p[3] == 0x03) {
            Some([0x03, 0x01, 0x01, if p[3] == 0x00 { 0x02 } else { 0x04 }])
        } else {
            None
        }
    }
}

/// The play state (0x47 DRC XML): the MCU looks for `<icon id="play">` and compares the next two letters [RE].
pub mod play {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Play {
        Playing,
        Paused,
        Empty,
    }
    impl Play {
        pub const fn icon(self) -> &'static str {
            match self {
                Play::Playing => ICON_PLAYING,
                Play::Paused => ICON_PAUSED,
                Play::Empty => ICON_EMPTY,
            }
        }
        pub fn from_icon(s: &[u8]) -> Option<Play> {
            [Play::Playing, Play::Paused, Play::Empty].into_iter().find(|p| s.starts_with(p.icon().as_bytes()))
        }
    }
    use crate::spec::play::{ICON_EMPTY, ICON_PAUSED, ICON_PLAYING};
    pub const PATTERN: &[u8] = crate::spec::play::PATTERN.as_bytes();
    /// The whole message, as the stock module sends it.
    #[inline]
    pub fn xml(p: Play, out: &mut impl FnMut(&[u8])) {
        out(b"<update id=\"status\">");
        out(PATTERN);
        out(p.icon().as_bytes());
        out(b"</icon></update>");
    }

    /// Finds the play icon in XML that arrives in pieces (256-byte notifications split tags anywhere).
    #[derive(Default)]
    pub struct Matcher {
        matched: usize,
        val: [u8; 3],
        vn: usize,
        in_val: bool,
    }
    impl Matcher {
        pub fn push(&mut self, c: u8) -> Option<Play> {
            if self.in_val {
                if c == b'<' || self.vn == self.val.len() {
                    self.in_val = false;
                    self.matched = if c == b'<' { 1 } else { 0 };
                    return Play::from_icon(&self.val[..self.vn]);
                }
                self.val[self.vn] = c;
                self.vn += 1;
                return None;
            }
            if c == PATTERN[self.matched] {
                self.matched += 1;
                if self.matched == PATTERN.len() {
                    self.in_val = true;
                    self.vn = 0;
                    self.matched = 0;
                }
            } else {
                self.matched = if c == PATTERN[0] { 1 } else { 0 };
            }
            None
        }
    }
}

/// The configuration channel (0x45): a write `[op][nameLen][name][value]`; P/Q/R read (the reply comes as a 0x45
/// notification), U/V/W set. [RE docs/protocol/spi-registers.md 3.7]
pub mod config {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Op {
        PersInt,
        PersStr,
        Cne,
        SetPersStr,
        SetPersInt,
        SetCne,
    }
    impl Op {
        pub fn from_byte(b: u8) -> Option<Op> {
            Some(match b {
                b'P' => Op::PersInt,
                b'Q' => Op::PersStr,
                b'R' => Op::Cne,
                b'U' => Op::SetPersStr,
                b'V' => Op::SetPersInt,
                b'W' => Op::SetCne,
                _ => return None,
            })
        }
        pub const fn byte(self) -> u8 {
            match self {
                Op::PersInt => b'P',
                Op::PersStr => b'Q',
                Op::Cne => b'R',
                Op::SetPersStr => b'U',
                Op::SetPersInt => b'V',
                Op::SetCne => b'W',
            }
        }
        /// A read: the module answers with a notification on 0x45.
        pub const fn is_read(self) -> bool {
            matches!(self, Op::PersInt | Op::PersStr | Op::Cne)
        }
    }
    pub struct Request<'a> {
        pub op: Op,
        pub name: &'a [u8],
        pub value: &'a [u8],
    }
    #[inline]
    pub fn parse(p: &[u8]) -> Option<Request<'_>> {
        let op = Op::from_byte(*p.first()?)?;
        let n = *p.get(1)? as usize;
        if p.len() < 2 + n {
            return None;
        }
        Some(Request { op, name: &p[2..2 + n], value: &p[2 + n..] })
    }
}

/// NetStatus (0x32): two ASCII characters.
pub mod net {
    pub const CONNECTED: [u8; 2] = *b"21";
    pub const DOWN: [u8; 2] = *b"00";
    /// The stock rule: both characters != '0' [RE 0x8000b518].
    #[inline]
    pub fn is_connected(s: [u8; 2]) -> bool {
        s[0] != b'0' && s[1] != b'0'
    }
}
