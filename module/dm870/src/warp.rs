//! The Warp cores' debug blocks: halt a core into debug mode, start it at an address, read its PC, park it
//! (docs/module/warp.md section 2: the sequences of Nokia's `warp_debugger.ko` [RE], driven by our Warp0 runtime
//! [HW 2026-10-24]). Every wait is bounded by a count of reads (an uncached read is ~0.3-1 us).

use crate::Mmio;

/// Warp0's debug block; Warp1's is the next page.
pub const DEBUG0: u32 = 0xE001_4000;
pub const DEBUG1: u32 = 0xE001_5000;

const EXEC: u32 = 0x00; // an instruction written here runs in debug mode
const DATA_IN: u32 = 0x04; // to the core: `mfc1 $x, $22`
const DATA_OUT: u32 = 0x08; // from the core: `mtc1 $x, $22`
const HALT: u32 = 0x0C; // bit 0 halt request (rw), bit 1 in debug mode (r)
const RESET: u32 = 0x10; // 0 held in reset, 1 released
const BUSY: u32 = 0x1C; // poll before and after each EXEC write

const MFC1_T0_RXTX: u32 = 0x4408_B000; // mfc1 $t0, $22
const MTC1_T0_DEBUGPC: u32 = 0x4488_A800; // mtc1 $t0, $21
const MTC1_ZERO_DEBUG: u32 = 0x4480_A000; // mtc1 $0, $20: leave debug mode
const MFC1_T0_PC: u32 = 0x4408_B800; // mfc1 $t0, $23
const MTC1_T0_RXTX: u32 = 0x4488_B000; // mtc1 $t0, $22

pub struct Debug<'a, M: Mmio + ?Sized> {
    m: &'a mut M,
    base: u32,
}

/// The debug block at `base` ([`DEBUG0`], [`DEBUG1`]).
pub fn debug<M: Mmio + ?Sized>(m: &mut M, base: u32) -> Debug<'_, M> {
    Debug { m, base }
}

impl<M: Mmio + ?Sized> Debug<'_, M> {
    fn r(&mut self, off: u32) -> u32 {
        self.m.r32(self.base + off)
    }
    fn w(&mut self, off: u32, v: u32) {
        self.m.w32(self.base + off, v)
    }

    fn idle(&mut self) -> bool {
        (0..100_000).any(|_| self.r(BUSY) == 0)
    }

    fn exec(&mut self, insn: u32) -> bool {
        if !self.idle() {
            return false;
        }
        self.w(EXEC, insn);
        self.idle()
    }

    fn in_debug_mode(&mut self) -> bool {
        (0..1000).any(|_| self.r(HALT) == 3)
    }

    /// Into debug mode from wherever it is: a halt request and a reset pulse. false: it didn't get there (stuck on a
    /// bus access, maybe until a power cycle).
    pub fn halt(&mut self) -> bool {
        self.w(HALT, 1);
        self.w(RESET, 0);
        self.w(RESET, 1);
        self.in_debug_mode()
    }

    /// From debug mode: run from `pc`.
    pub fn start(&mut self, pc: u32) -> bool {
        self.w(DATA_IN, pc);
        if !self.exec(MFC1_T0_RXTX) || !self.exec(MTC1_T0_DEBUGPC) {
            return false;
        }
        self.w(HALT, 0);
        self.exec(MTC1_ZERO_DEBUG)
    }

    /// Stop it where it is (no reset) and read its PC through debug mode; it stays halted (park it, or start it).
    /// None: it doesn't enter debug mode (stuck on a bus access: there is no PC to be had).
    pub fn inspect(&mut self) -> Option<u32> {
        self.w(HALT, 1);
        if !self.in_debug_mode() || !self.exec(MFC1_T0_PC) || !self.exec(MTC1_T0_RXTX) {
            return None;
        }
        Some(self.r(DATA_OUT))
    }

    /// Held in reset, as KnOS leaves it; also what recovers a core hung on a bus access (section 7 has the limits).
    pub fn park(&mut self) {
        self.w(HALT, 1);
        self.w(RESET, 1);
        self.w(RESET, 0);
        self.w(HALT, 0);
    }
}
