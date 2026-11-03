//! The pad switch matrix (0xE0013000): which on-chip function owns which pads. Named after KnOS's `sys swmtx`
//! (re/netmodule/shell-dumps/emu_sys_regs.txt). The block repeats 16 times, 0x100 apart, and every copy is written.
//!
//! BL2 and KnOS set it at start-up; the Nokia kernel never does. Without it the SPI slave's pads, the request line to
//! the MCU and the GPIO inputs are cut off, and the Ethernet PHY doesn't answer [HW, JTAG 2026-10-23]. Who sets it:
//! the boot stub (bare metal, before the kernel), and init under Linux (`zd1 board up`), once.

use crate::Mmio;

pub const BASE: u32 = 0xE001_3000;
pub const COPIES: u32 = 16;
const STRIDE: u32 = 0x100;

/// A register of the matrix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Mtx {
    /// Which function owns each GPIO pad group.
    GpioGroupA = 0x00,
    GpioGroupB = 0x04,
    GpioGroupC = 0x08,
    GpioGroupD = 0x0C,
    GpioGroupE = 0x10,
    GpioGroupF = 0x14,
    Usb = 0x20,
    Ethernet = 0x24,
    /// The Wi-Fi core's. `sulu_wlan` clears it while it starts the core; set again afterwards (BL2's 1) is fine:
    /// Wi-Fi associated with it at 1 [HW 2026-10-26].
    Wild = 0x28,
    Others = 0x2C,
    AllIns = 0x30,
    AvLoop = 0x34,
    TestBusSel = 0x40,
    TestBusEnable = 0x44,
}

impl Mtx {
    pub const ALL: [Mtx; 14] = [
        Mtx::GpioGroupA,
        Mtx::GpioGroupB,
        Mtx::GpioGroupC,
        Mtx::GpioGroupD,
        Mtx::GpioGroupE,
        Mtx::GpioGroupF,
        Mtx::Usb,
        Mtx::Ethernet,
        Mtx::Wild,
        Mtx::Others,
        Mtx::AllIns,
        Mtx::AvLoop,
        Mtx::TestBusSel,
        Mtx::TestBusEnable,
    ];

    /// KnOS's name.
    pub fn name(self) -> &'static str {
        match self {
            Mtx::GpioGroupA => "MtxGpioGroupA",
            Mtx::GpioGroupB => "MtxGpioGroupB",
            Mtx::GpioGroupC => "MtxGpioGroupC",
            Mtx::GpioGroupD => "MtxGpioGroupD",
            Mtx::GpioGroupE => "MtxGpioGroupE",
            Mtx::GpioGroupF => "MtxGpioGroupF",
            Mtx::Usb => "MtxUSB",
            Mtx::Ethernet => "MtxEthernet",
            Mtx::Wild => "MtxWild",
            Mtx::Others => "MtxOthers",
            Mtx::AllIns => "MtxAllIns",
            Mtx::AvLoop => "MtxAvLoop",
            Mtx::TestBusSel => "MtxTestBusSel",
            Mtx::TestBusEnable => "MtxTestBusEnable",
        }
    }
}

/// Values for some of the matrix's registers, written in this order (the rest left as they are).
pub struct Routing(pub &'static [(Mtx, u32)]);

/// BL2's routing, in BL2's order [HW, JTAG 2026-10-23].
pub const BL2: Routing = Routing(&[
    (Mtx::GpioGroupA, 0x102),
    (Mtx::GpioGroupB, 0x703),
    (Mtx::GpioGroupC, 0xF01),
    (Mtx::GpioGroupD, 0x706),
    (Mtx::GpioGroupE, 0x103),
    (Mtx::GpioGroupF, 0x307),
    (Mtx::Usb, 0x20),
    (Mtx::Ethernet, 1),
    (Mtx::Wild, 1),
    (Mtx::AllIns, 0),
]);

pub struct Pads<'a, M: Mmio + ?Sized>(&'a mut M);

pub fn pads<M: Mmio + ?Sized>(m: &mut M) -> Pads<'_, M> {
    Pads(m)
}

impl<M: Mmio + ?Sized> Pads<'_, M> {
    /// Write a routing into every copy of the block.
    pub fn route(&mut self, r: &Routing) {
        for copy in 0..COPIES {
            for &(reg, v) in r.0 {
                self.0.w32(BASE + copy * STRIDE + reg as u32, v);
            }
        }
    }

    pub fn get(&mut self, copy: u32, reg: Mtx) -> u32 {
        self.0.r32(BASE + copy * STRIDE + reg as u32)
    }

    /// Whether every copy holds the routing.
    pub fn holds(&mut self, r: &Routing) -> bool {
        (0..COPIES).all(|c| r.0.iter().all(|&(reg, v)| self.get(c, reg) == v))
    }
}
