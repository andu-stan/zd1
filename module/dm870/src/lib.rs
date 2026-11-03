//! The DM870 by name: the network module's SoC, as far as our code touches it. Every address, field and value lives
//! here once, with what it does and how we know: the boot stub, the MCU link, `zd1 board`, init and the Linux tools all
//! reach the chip through this crate. `docs/module/memory-map.md` describes the same map with the evidence; where the
//! two disagree, the unit decides and both get fixed.
//!
//! A bus ([`Mmio`]) reaches the registers: [`Physical`] on bare metal (the MMU off), [`DevMem`] under Linux (feature
//! `linux`). Each block is a view over a bus, opened with its function:
//!
//! ```ignore
//! clocks::tgen(bus).enable(Gates::STOCK_APP);
//! gpio::gpio(bus).drive(Pin::NresetEth, Level::High);     // the PHY out of reset
//! pads::pads(bus).route(&pads::BL2);
//! ```
//!
//! Some registers are known only by the value the stock firmware writes. They are named for where they are, the
//! value for who wrote it, and what they do is said to be unknown: better that than a guess that reads like fact.
#![cfg_attr(not(any(test, feature = "linux")), no_std)]

pub mod clocks;
#[cfg(feature = "linux")]
mod devmem;
pub mod gpio;
pub mod mailbox;
pub mod memory;
pub mod pads;
pub mod spi_slave;
pub mod timers;
pub mod uart;
pub mod warp;
#[cfg(feature = "linux")]
pub use devmem::{DevMem, Region};

/// Register access at physical addresses.
pub trait Mmio {
    fn r32(&mut self, pa: u32) -> u32;
    fn w32(&mut self, pa: u32, v: u32);
    fn r8(&mut self, pa: u32) -> u8;
    fn w8(&mut self, pa: u32, v: u8);
}

impl<M: Mmio + ?Sized> Mmio for &mut M {
    fn r32(&mut self, pa: u32) -> u32 {
        (**self).r32(pa)
    }
    fn w32(&mut self, pa: u32, v: u32) {
        (**self).w32(pa, v)
    }
    fn r8(&mut self, pa: u32) -> u8 {
        (**self).r8(pa)
    }
    fn w8(&mut self, pa: u32, v: u8) {
        (**self).w8(pa, v)
    }
}

/// The registers at their physical addresses: bare metal with the MMU off, as the preloader leaves it.
pub struct Physical;

impl Mmio for Physical {
    fn r32(&mut self, pa: u32) -> u32 {
        unsafe { core::ptr::read_volatile(pa as *const u32) }
    }
    fn w32(&mut self, pa: u32, v: u32) {
        unsafe { core::ptr::write_volatile(pa as *mut u32, v) }
    }
    fn r8(&mut self, pa: u32) -> u8 {
        unsafe { core::ptr::read_volatile(pa as *const u8) }
    }
    fn w8(&mut self, pa: u32, v: u8) {
        unsafe { core::ptr::write_volatile(pa as *mut u8, v) }
    }
}

/// Restart the chip into BL2: the mailbox BL1 reads, then the watchdog. The stock app's way
/// (docs/module/boot-chain.md).
pub fn reset_into_bl2(m: &mut impl Mmio) -> ! {
    mailbox::request_bl2(m);
    timers::timers(m).reset_now()
}
