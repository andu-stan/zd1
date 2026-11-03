//! GPIO (0xFFFF0400): 20 pads, two masks [HW, JTAG 2026-10-25; docs/module/memory-map.md, GPIO].
//!
//! - **A**, whether a pad is left alone: set = not driven, clear = it drives B.
//! - **B**, what a driven pad outputs, and a released pad's pull-up.
//!
//! "Output high" in the Linux sense (A and B set) only releases the pad onto its pull-up, which the board's pull-downs
//! on ETH NRESET and the request line beat: that held both low. To hold a line high, drive it.

use crate::Mmio;

pub const BASE: u32 = 0xFFFF_0400;

/// The registers, from [`BASE`]. Each write touches only the pads whose bits are set.
pub mod reg {
    /// A set: the pads are left alone. Reads A.
    pub const RELEASE: u32 = 0x00;
    /// A clear: the pads drive B. Reads A.
    pub const DRIVE: u32 = 0x04;
    /// B set. Reads the pads' levels.
    pub const HIGH: u32 = 0x08;
    /// B clear. Reads B.
    pub const LOW: u32 = 0x0C;
}
use reg::{DRIVE, HIGH, LOW, RELEASE};

/// The pads, named as the Nokia kernel's GPIO table names the DM870's pins (`ce2_init` → `gpio_init`), which matches
/// the CR870 datasheet and this carrier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Pin {
    X17P17 = 0,
    X17P15 = 1,
    X17P13 = 2,
    /// MIITXCLK; the iPod accessory power on J2.
    X17P11 = 3,
    /// The factory-reset strap (an input, pulled down on this carrier).
    FactReset = 4,
    Infrared = 5,
    /// The request line to the MCU (its PA20, "BCO IRQ"): high while the link has something for it.
    HostNireq = 6,
    X17P31 = 7,
    /// 8-11: Nokia's bit-banged shift-register SPI; the stock app uses 8 and 10 as a second I2C.
    SrSpiMosi = 8,
    SrSpiMiso = 9,
    SrSpiClk = 10,
    SrSpiCs = 11,
    /// The Ethernet PHY's reset, active low (J2 pin 72; pulled down on the carrier).
    NresetEth = 12,
    /// 13/14: the I2C bus to the Apple authentication chip.
    I2cScl = 13,
    I2cSda = 14,
    X17P27 = 15,
    LcdA0 = 16,
    P29 = 17,
    IrqTunerMod = 18,
    IpodNdetect = 19,
}

impl Pin {
    pub const ALL: [Pin; 20] = [
        Pin::X17P17,
        Pin::X17P15,
        Pin::X17P13,
        Pin::X17P11,
        Pin::FactReset,
        Pin::Infrared,
        Pin::HostNireq,
        Pin::X17P31,
        Pin::SrSpiMosi,
        Pin::SrSpiMiso,
        Pin::SrSpiClk,
        Pin::SrSpiCs,
        Pin::NresetEth,
        Pin::I2cScl,
        Pin::I2cSda,
        Pin::X17P27,
        Pin::LcdA0,
        Pin::P29,
        Pin::IrqTunerMod,
        Pin::IpodNdetect,
    ];

    /// The Nokia kernel's name.
    pub fn name(self) -> &'static str {
        match self {
            Pin::X17P17 => "GPIO_X17_P17",
            Pin::X17P15 => "GPIO_X17_P15",
            Pin::X17P13 => "GPIO_X17_P13",
            Pin::X17P11 => "GPIO_X17_P11",
            Pin::FactReset => "fact_reset",
            Pin::Infrared => "infrared",
            Pin::HostNireq => "host_nireq",
            Pin::X17P31 => "GPIO_X17_P31",
            Pin::SrSpiMosi => "sr_spi_mosi",
            Pin::SrSpiMiso => "sr_spi_miso",
            Pin::SrSpiClk => "sr_spi_clk",
            Pin::SrSpiCs => "sr_spi_cs",
            Pin::NresetEth => "nreset_eth",
            Pin::I2cScl => "i2c_scl",
            Pin::I2cSda => "i2c_sda",
            Pin::X17P27 => "GPIO_X17_P27",
            Pin::LcdA0 => "lcd_a0",
            Pin::P29 => "GPIO_P29",
            Pin::IrqTunerMod => "irq_tuner_mod",
            Pin::IpodNdetect => "ipod_ndetect",
        }
    }

    pub fn bit(self) -> u32 {
        1 << self as u8
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Low,
    High,
}

impl From<bool> for Level {
    fn from(high: bool) -> Level {
        if high { Level::High } else { Level::Low }
    }
}

/// What a pad does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Drives(Level),
    /// Left alone, with or without its pull-up.
    Released {
        pull_up: bool,
    },
}

pub struct Gpio<'a, M: Mmio + ?Sized>(&'a mut M);

pub fn gpio<M: Mmio + ?Sized>(m: &mut M) -> Gpio<'_, M> {
    Gpio(m)
}

impl<M: Mmio + ?Sized> Gpio<'_, M> {
    fn b(&mut self, pin: Pin, high: bool) {
        self.0.w32(BASE + if high { HIGH } else { LOW }, pin.bit())
    }

    /// Drive a pad: the level first, then let it drive (no glitch to the other level).
    pub fn drive(&mut self, pin: Pin, level: Level) {
        self.b(pin, level == Level::High);
        self.0.w32(BASE + DRIVE, pin.bit());
    }

    /// Leave a pad alone, with or without its pull-up.
    pub fn release(&mut self, pin: Pin, pull_up: bool) {
        self.b(pin, pull_up);
        self.0.w32(BASE + RELEASE, pin.bit());
    }

    /// The pad's level, driven or not.
    pub fn level(&mut self, pin: Pin) -> Level {
        (self.0.r32(BASE + HIGH) & pin.bit() != 0).into()
    }

    pub fn mode(&mut self, pin: Pin) -> Mode {
        let released = self.0.r32(BASE + RELEASE) & pin.bit() != 0;
        let b = self.0.r32(BASE + LOW) & pin.bit() != 0;
        if released { Mode::Released { pull_up: b } } else { Mode::Drives(b.into()) }
    }
}
