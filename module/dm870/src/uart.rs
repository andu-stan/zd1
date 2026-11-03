//! The UARTs (0xB1000000, 0xB1001000) and the FIFO port they share with the SPI slave and the NAND controller: RX and
//! TX FIFOs behind fixed windows, their fill levels, a status word.

use crate::Mmio;

pub const UART0: u32 = 0xB100_0000;
/// The console: the stock shell's port, J2 pins 35/37, wired to the MCU's USART1 (which logs it).
pub const UART1: u32 = 0xB100_1000;

/// The FIFO port's layout, from a block's base. Names after KnOS's (`iFifo...`, re/netmodule/shell-dumps/
/// sys_spi_reg.txt), in the chip's set/clear pairs.
pub mod fifo {
    pub const RX_DATA: u32 = 0xE00;
    pub const TX_DATA: u32 = 0xE80;
    /// Written (0) when the stock driver resets its receive side: empties the RX FIFO, by the look of it [RE].
    pub const RX_RESET: u32 = 0xF00;
    /// The same for transmit.
    pub const TX_RESET: u32 = 0xF04;
    /// iFifoFillLevelThdRx / iFifoEmptyLevelThdTx: the IRQ thresholds.
    pub const RX_THRESHOLD: u32 = 0xF10;
    pub const TX_THRESHOLD: u32 = 0xF14;
    /// iFifoFillLevelRx: bytes waiting in the RX FIFO.
    pub const RX_FILL: u32 = 0xF18;
    /// iFifoEmptyLevelTx: room in the TX FIFO.
    pub const TX_FREE: u32 = 0xF1C;
    /// iFifoTimer / iFifoTimerThd.
    pub const TIMER: u32 = 0xF20;
    pub const TIMER_THRESHOLD: u32 = 0xF24;
    /// iFifoCfgSet / Clr.
    pub const CFG_SET: u32 = 0xF30;
    pub const CFG_CLEAR: u32 = 0xF34;
    /// iFifoStatSet; the status reads at +0xF44.
    pub const STAT_SET: u32 = 0xF40;
    pub const STATUS: u32 = 0xF44;
    pub const STATUS_TX_FULL: u32 = 1 << 1;
    /// iFifoIrqMaskSet / Clr.
    pub const IRQ_MASK_SET: u32 = 0xF50;
    pub const IRQ_MASK_CLEAR: u32 = 0xF54;
}

const BAUD_DIVISOR: u32 = 0x08; // baud = 2 × clock / (divisor + 2)

/// The UARTs' clock after the preloader: BL1 programs the console for its own 24 MHz, then the preloader raises the
/// clock, so a bare-metal image sets the divisor again or its text comes out at ~383 kbaud [HW 2026-10-26].
pub const CLOCK_HZ: u32 = 80_000_000;

/// The divisor for a baud rate, rounded down as Linux does (115200 at 80 MHz: 0x56A).
pub const fn divisor(clock_hz: u32, baud: u32) -> u32 {
    2 * clock_hz / baud - 2
}

pub struct Uart<'a, M: Mmio + ?Sized>(&'a mut M, u32);

pub fn uart<M: Mmio + ?Sized>(m: &mut M, base: u32) -> Uart<'_, M> {
    Uart(m, base)
}

impl<M: Mmio + ?Sized> Uart<'_, M> {
    pub fn set_baud(&mut self, baud: u32) {
        self.0.w32(self.1 + BAUD_DIVISOR, divisor(CLOCK_HZ, baud))
    }

    /// One byte out, once there is room.
    pub fn put(&mut self, b: u8) {
        while self.0.r32(self.1 + fifo::STATUS) & fifo::STATUS_TX_FULL != 0 {}
        self.0.w8(self.1 + fifo::TX_DATA, b);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_console_divisor_is_linux_s() {
        assert_eq!(super::divisor(super::CLOCK_HZ, 115_200), 0x56A);
    }
}
