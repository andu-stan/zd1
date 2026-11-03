//! The SPI slave (0xB1002000): the link to the MCU. Polled (no kernel driver binds it: zd1-linkd reaches it through
//! /dev/mem); the MCU's side of the conversation is the request line, [`crate::gpio::Pin::HostNireq`]. Registers named
//! as KnOS's `sys spi reg` names them (re/netmodule/shell-dumps/sys_spi_reg.txt), in the chip's set/clear pairs; the
//! configuration word decoded from the stock app's driver (`FUN_6032152c`, fed by its `sys spi config` options:
//! `-cpo -cph -csp -msb -fnc -nop -dwh -bdr -mod`) [RE].

use crate::Mmio;
use crate::uart::fifo;

pub const BASE: u32 = 0xB100_2000;

/// The registers, from [`BASE`]. The FIFO port above 0xE00 is the UARTs' ([`crate::uart::fifo`]).
pub mod reg {
    /// iSpiCfgSet / Clr: the configuration ([`super::Config`]) and bit 0, on.
    pub const CFG_SET: u32 = 0x000;
    pub const CFG_CLEAR: u32 = 0x004;
    /// iSpiDiv: the clock divisor when master, ⌈clock / baud⌉ − 2 (a slave runs on the master's clock).
    pub const DIVISOR: u32 = 0x008;
    /// iSpiNop: the nop character, clocked out while the TX FIFO is empty (and dropped from RX with the filter on).
    pub const NOP: u32 = 0x00C;
    /// iSpiWd: the driver writes 0; what it does is unknown.
    pub const WD: u32 = 0x010;
    pub const STAT_SET: u32 = 0x020;
    pub const IRQ_MASK_SET: u32 = 0x030;
}

/// Configuration bit 0: the slave runs.
const ON: u32 = 1;

/// Word width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
    Bits8 = 0,
    Bits16 = 1,
    Bits32 = 2,
}

/// The configuration word's fields, as the stock driver packs them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub master: bool,
    pub width: Width,
    /// Received bytes equal to the nop character are dropped.
    pub filter_nop: bool,
    /// Chip-select polarity (`-csp`): 0 sets bits 12-15, 1 bit 21.
    pub cs_polarity: bool,
    /// Clock phase, CPHA (`-cph`): bits 17-19.
    pub clock_phase: bool,
    /// Clock polarity, CPOL (`-cpo`): bit 20.
    pub clock_polarity: bool,
    pub msb_first: bool,
}

impl Config {
    /// The MCU link, as the stock app sets it (0x5EF100): a slave, 8-bit, SPI mode 3 (our MCU's master is mode 3 too:
    /// mcu/src/dm870.c), MSB first, the MCU's idle bytes (the nop character) not received.
    pub const LINK: Config = Config {
        master: false,
        width: Width::Bits8,
        filter_nop: true,
        cs_polarity: false,
        clock_phase: true,
        clock_polarity: true,
        msb_first: true,
    };

    pub const fn word(self) -> u32 {
        let mut w = (self.width as u32) << 4;
        if self.master {
            w |= 1 << 1;
        }
        if self.filter_nop {
            w |= 1 << 8;
        }
        w |= if self.cs_polarity { 1 << 21 } else { 0xF << 12 };
        if self.clock_phase {
            w |= 0x7 << 17;
        }
        if self.clock_polarity {
            w |= 1 << 20;
        }
        if self.msb_first {
            w |= 1 << 22;
        }
        w
    }
}

/// The idle and nop character.
pub const NOP_CHARACTER: u8 = 0xFF;
/// The stock divisor: 1 kHz at the 80 MHz bus clock (the driver's default baud; unused by a slave).
pub const STOCK_DIVISOR: u32 = 80_000 - 2;

/// The FIFO port's settings, as the stock driver leaves them (KnOS reads the same): IRQ thresholds and the FIFO
/// timer's, then the FIFO configuration = 1 (bit 0; what it switches is unknown).
const FIFO_TIMER_THRESHOLD: u32 = 0x10;
const RX_THRESHOLD: u32 = 4;
const TX_THRESHOLD: u32 = 0x3B;
const FIFO_CONFIG: u32 = 1;

/// TX FIFO depth.
pub const TX_DEPTH: u32 = 63;

pub struct SpiSlave<'a, M: Mmio + ?Sized>(&'a mut M);

pub fn spi_slave<M: Mmio + ?Sized>(m: &mut M) -> SpiSlave<'_, M> {
    SpiSlave(m)
}

impl<M: Mmio + ?Sized> SpiSlave<'_, M> {
    fn w(&mut self, off: u32, v: u32) {
        self.0.w32(BASE + off, v)
    }

    /// Set it up as the stock driver does, in its order: off, both FIFOs emptied, the configuration
    /// ([`Config::LINK`]), the nop character, on, then the FIFO port. The pads must be routed
    /// ([`crate::pads::BL2`]) and its clock on ([`crate::clocks::Gates::SPI_SLAVE`]).
    pub fn init(&mut self) {
        self.w(reg::CFG_CLEAR, ON);
        self.w(fifo::RX_RESET, 0);
        self.w(fifo::TX_RESET, 0);
        self.w(reg::DIVISOR, STOCK_DIVISOR);
        self.w(reg::WD, 0);
        self.w(reg::CFG_SET, Config::LINK.word());
        self.w(reg::CFG_CLEAR, !Config::LINK.word());
        self.w(reg::NOP, NOP_CHARACTER as u32);
        self.w(reg::CFG_SET, ON);
        self.w(fifo::TIMER_THRESHOLD, FIFO_TIMER_THRESHOLD);
        self.w(fifo::RX_THRESHOLD, RX_THRESHOLD);
        self.w(fifo::TX_THRESHOLD, TX_THRESHOLD);
        self.w(fifo::CFG_CLEAR, !0);
        self.w(fifo::CFG_SET, FIFO_CONFIG);
    }

    pub fn rx(&mut self) -> Option<u8> {
        if self.0.r32(BASE + fifo::RX_FILL) == 0 {
            return None;
        }
        Some(self.0.r8(BASE + fifo::RX_DATA))
    }

    pub fn tx_free(&mut self) -> u32 {
        self.0.r32(BASE + fifo::TX_FREE)
    }

    /// One byte into the TX FIFO; false when full.
    pub fn tx(&mut self, b: u8) -> bool {
        if self.tx_free() == 0 {
            return false;
        }
        self.0.w8(BASE + fifo::TX_DATA, b);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_s_configuration_is_the_stock_word() {
        assert_eq!(Config::LINK.word(), 0x005E_F100);
    }

    /// What the stock app's init wrote, from its MMIO trace (app 0x60321548..0x603217F0): `init` must write the same.
    #[test]
    fn init_writes_the_stock_sequence() {
        const STOCK: [(u32, u32); 14] = [
            (0x004, 1),
            (0xF00, 0),
            (0xF04, 0),
            (0x008, 0x0001_387E),
            (0x010, 0),
            (0x000, 0x005E_F100),
            (0x004, !0x005E_F100u32),
            (0x00C, 0xFF),
            (0x000, 1),
            (0xF24, 0x10),
            (0xF10, 4),
            (0xF14, 0x3B),
            (0xF34, 0xFFFF_FFFF),
            (0xF30, 1),
        ];
        struct Log(std::vec::Vec<(u32, u32)>);
        impl Mmio for Log {
            fn r32(&mut self, _: u32) -> u32 {
                0
            }
            fn w32(&mut self, pa: u32, v: u32) {
                self.0.push((pa - BASE, v))
            }
            fn r8(&mut self, _: u32) -> u8 {
                0
            }
            fn w8(&mut self, _: u32, _: u8) {}
        }
        let mut log = Log(std::vec::Vec::new());
        spi_slave(&mut log).init();
        assert_eq!(log.0, STOCK);
    }
}
