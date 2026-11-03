//! The clock generator, "tgen" (0xE0007000): the system PLL and its dividers, block resets, clock gates, the AV PLLs.
//! Decoded from the Nokia kernel's tgen driver (`tgen_init`, `tgen_get_clock`) and checked against the live registers
//! [HW 2026-10-23] (docs/module/memory-map.md, Clocks).

use crate::Mmio;

pub const BASE: u32 = 0xE000_7000;

const SYS_PLL: u32 = 0x000; // root = 24 MHz × [15:8] / [20:16] / 2^[1:0]
const AHB_DIVIDER: u32 = 0x010; // AHB = root / this
const DOMAINS: u32 = 0x014; // one 2-bit multiplier of the AHB clock per domain (0 = ×4)
const RESET_ASSERT: u32 = 0x020; // write: assert those bits; read: the asserted ones
const RESET_RELEASE: u32 = 0x024;
const GATE_ENABLE: u32 = 0x028; // write: switch those gates on; read: the enable mask (see Gates::readback)
const GATE_DISABLE: u32 = 0x02C;
const MISC: u32 = 0x040;
/// The PLLs' status words (InLock, PhErr, IIRStat) from here, 0x10 apart.
pub const PLL_STATUS: u32 = 0x800;

/// The crystal.
pub const XTAL_HZ: u32 = 24_000_000;

/// A clock gate: `sulu_power_enable(n)` in the Nokia kernel, n ≤ 25. The named ones are attributed from the Nokia
/// drivers [RE] (Ethernet's also by switching it live [HW 2026-10-23]); the kernel's board init keeps gates 0, 2, 4, 5,
/// 7, 9-11, 15, 16, 23-25 on without saying what they feed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gate(pub u8);

impl Gate {
    pub const WARP0: Gate = Gate(2);
    /// The AV ports and the PTU (`av-device`).
    pub const AV: Gate = Gate(3);
    /// The Ethernet MAC (`sulu_eth`): off, the link is dead.
    pub const ETHERNET: Gate = Gate(7);
    pub const USB_HOST: Gate = Gate(8);
    pub const NAND: Gate = Gate(13);
    /// The LCD controller (`sulufb`): no display on this board.
    pub const LCD: Gate = Gate(17);
    /// The Wi-Fi core's ARM7 (`sulu_wlan`).
    pub const WILD: Gate = Gate(19);
    pub const WARP1: Gate = Gate(20);
    pub const SOL: Gate = Gate(21);
    /// Warp1's and Sol's window (`wsl`).
    pub const WARP_WINDOW: Gate = Gate(22);
    /// The highest gate the kernel knows.
    pub const LAST: Gate = Gate(25);

    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Gate::WARP0 => "Warp0",
            Gate::AV => "AV ports / PTU",
            Gate::ETHERNET => "Ethernet MAC",
            Gate::USB_HOST => "USB host",
            Gate::NAND => "NAND",
            Gate::LCD => "LCD",
            Gate::WILD => "Wi-Fi ARM7",
            Gate::WARP1 => "Warp1",
            Gate::SOL => "Sol",
            Gate::WARP_WINDOW => "Warp1/Sol window",
            _ => return None,
        })
    }
}

/// A set of clock gates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Gates(pub u32);

impl Gates {
    pub const fn of(list: &[Gate]) -> Gates {
        let mut m = 0;
        let mut i = 0;
        while i < list.len() {
            m |= 1 << list[i].0;
            i += 1;
        }
        Gates(m)
    }
    pub const fn and(self, other: Gates) -> Gates {
        Gates(self.0 | other.0)
    }
    pub fn contains(self, g: Gate) -> bool {
        self.0 & 1 << g.0 != 0
    }
    pub fn iter(self) -> impl Iterator<Item = Gate> {
        (0..32).filter(move |n| self.0 & 1 << n != 0).map(Gate)
    }

    /// What the stock app switches on beyond the drivers': 6, 12, 14 and 18 (not attributed) and Warp1, Sol and their
    /// window. The Nokia kernel switches every gate it has no driver for off at boot; nothing in Linux owns these.
    pub const STOCK_APP: Gates =
        Gates::of(&[Gate(6), Gate(12), Gate(14), Gate(18), Gate::WARP1, Gate::SOL, Gate::WARP_WINDOW]);
    /// What the SPI slave (the MCU link) needs on top of [`Gates::STOCK_APP`]: one of AV or WILD. Under the system
    /// their drivers switch them on; the rescue loads no drivers [HW 2026-10-26, JTAG: with BL2's gates the slave
    /// received, without either of these it took no byte].
    pub const SPI_SLAVE: Gates = Gates::of(&[Gate::AV, Gate::WILD]);
    /// Gates whose enable bit reads 0 whatever is written [HW 2026-10-25]: the readback is the enable mask but these.
    pub const READ_AS_ZERO: Gates = Gates::of(&[Gate(1), Gate(4), Gate(24)]);
}

/// Block resets (the reset register's bits).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reset(pub u32);

impl Reset {
    /// The Wi-Fi core's ARM7. BL2 leaves it asserted; `sulu_wlan` asserts it while it loads the firmware and releases
    /// it after. Asserting it behind the driver's back holds Wi-Fi off until released by hand [HW 2026-10-23].
    pub const WILD_ARM7: Reset = Reset(1 << 1);
}

/// The misc register's bits.
pub mod misc {
    /// Cleared by `sulu_wlan` before the Wi-Fi core starts, and it must stay as the driver left it: set again after
    /// the driver has loaded (BL2's value), the core runs but hears nothing until the driver is reloaded [HW
    /// 2026-10-24].
    pub const WILD_HOLD: u32 = 1 << 15;
    /// The stock app sets it; what it does is unknown.
    pub const STOCK: u32 = 1 << 21;
}

/// Registers the stock app writes whose purpose is unknown, with its values. Nothing has been seen to need them; they
/// are written to keep the chip where the stock app had it.
pub const STOCK_UNKNOWN: [(u32, u32); 3] = [(0x050, 0x3F), (0x600, 0x8000), (0x604, 0)];

/// One of tgen's PLLs: a control word (bit 31 runs it) and two configuration words (the frequency's fields:
/// docs/module/memory-map.md, Clocks).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pll(u32);

impl Pll {
    /// "av_clock": the audio clock, programmed per sample rate by `av-device` (45.1584 MHz = 1024 × 44.1 kHz).
    pub const AV_CLOCK: Pll = Pll(0x140);
    /// AV1: on this board, the Ethernet PHY's clock. Its MDIO answers only with it running; stopping it drops the
    /// link for good (the driver doesn't renegotiate) [HW 2026-10-23].
    pub const AV1: Pll = Pll(0x240);
}

/// A PLL's three words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PllSetting {
    pub control: u32,
    pub config: [u32; 2],
}

impl PllSetting {
    /// AV1 as BL2 leaves it: the Ethernet PHY's clock.
    pub const AV1_PHY: PllSetting = PllSetting { control: 0x8005_2666, config: [0x100D_0802, 0x0011_0002] };
    pub fn running(&self) -> bool {
        self.control & 1 << 31 != 0
    }
}

/// The clock tree as programmed: the root and the AHB clock, and each domain's clock.
#[derive(Clone, Copy, Debug)]
pub struct Tree {
    pub root_hz: u32,
    pub ahb_hz: u32,
    pub domains: [(&'static str, u32); 8],
}

/// The domains in the multiplier word's order, 4 bits apart.
const DOMAIN_NAMES: [&str; 8] = ["ARM", "Warp0", "SDRAM", "AV system", "HV system", "AV oversampling", "Warp1", "Sol"];

pub struct Tgen<'a, M: Mmio + ?Sized>(&'a mut M);

pub fn tgen<M: Mmio + ?Sized>(m: &mut M) -> Tgen<'_, M> {
    Tgen(m)
}

impl<M: Mmio + ?Sized> Tgen<'_, M> {
    fn r(&mut self, off: u32) -> u32 {
        self.0.r32(BASE + off)
    }
    fn w(&mut self, off: u32, v: u32) {
        self.0.w32(BASE + off, v)
    }

    /// Switch gates on; the others stay as they are.
    pub fn enable(&mut self, g: Gates) {
        self.w(GATE_ENABLE, g.0)
    }
    /// Switch gates off; the others stay as they are.
    pub fn disable(&mut self, g: Gates) {
        self.w(GATE_DISABLE, g.0)
    }
    /// The gates that are on (but [`Gates::READ_AS_ZERO`]).
    pub fn gates(&mut self) -> Gates {
        Gates(self.r(GATE_ENABLE))
    }

    pub fn assert(&mut self, r: Reset) {
        self.w(RESET_ASSERT, r.0)
    }
    pub fn release(&mut self, r: Reset) {
        self.w(RESET_RELEASE, r.0)
    }
    pub fn in_reset(&mut self, r: Reset) -> bool {
        self.r(RESET_ASSERT) & r.0 != 0
    }

    pub fn misc(&mut self) -> u32 {
        self.r(MISC)
    }
    /// The stock app's misc bits, [`misc::WILD_HOLD`] kept as it is.
    pub fn set_misc_stock(&mut self) {
        let hold = self.r(MISC) & misc::WILD_HOLD;
        self.w(MISC, hold | misc::STOCK);
    }

    pub fn write_stock_unknown(&mut self) {
        for (off, v) in STOCK_UNKNOWN {
            self.w(off, v);
        }
    }

    /// Program a PLL: the configuration first, the control word (which runs it) last.
    pub fn program(&mut self, pll: Pll, s: PllSetting) {
        self.w(pll.0 + 4, s.config[0]);
        self.w(pll.0 + 8, s.config[1]);
        self.w(pll.0, s.control);
    }
    pub fn pll(&mut self, pll: Pll) -> PllSetting {
        PllSetting { control: self.r(pll.0), config: [self.r(pll.0 + 4), self.r(pll.0 + 8)] }
    }

    pub fn tree(&mut self) -> Tree {
        let pll = self.r(SYS_PLL);
        let (mul, div, post) = ((pll >> 8) & 0xFF, ((pll >> 16) & 0x1F).max(1), pll & 3);
        let root_hz = ((XTAL_HZ as u64 * mul as u64 / div as u64) >> post) as u32;
        let ahb_hz = root_hz / self.r(AHB_DIVIDER).max(1);
        let m = self.r(DOMAINS);
        let mut domains = [("", 0); 8];
        for (i, d) in domains.iter_mut().enumerate() {
            let k = (m >> (4 * i)) & 3;
            *d = (DOMAIN_NAMES[i], ahb_hz * if k == 0 { 4 } else { k });
        }
        Tree { root_hz, ahb_hz, domains }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stock_gates_are_the_word_s05pads_wrote() {
        assert_eq!(Gates::STOCK_APP.0, 0x0074_5040);
    }

    #[test]
    fn the_rescue_s_gates_are_the_word_its_init_wrote() {
        assert_eq!(Gates::SPI_SLAVE.0, 0x0008_0008);
    }
}
