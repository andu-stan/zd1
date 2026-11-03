//! The timers (0xFFFF0200): three countdown timers and a free-running timestamp, all at the 80 MHz timer clock.
//! KnOS ticks on timer 1 (1 ms), Linux on timer 2 (10 ms; the timestamp is its clocksource). **Timer 0 is the chip's
//! watchdog and its only software reset**: with [`RESET_ON_EXPIRY`] it restarts the chip through BL1 when it runs out
//! (Linux's `reboot` does exactly that) [HW 2026-10-23].

use crate::Mmio;

pub const BASE: u32 = 0xFFFF_0200;

const RUN: u32 = 0x00; // write: set these configuration bits
const STOP: u32 = 0x04; // write: clear them
const COUNT: u32 = 0x10; // + 4 × timer
const THRESHOLD: u32 = 0x20; // + 4 × timer
const TIMESTAMP: u32 = 0x30;

/// Configuration bits: timer n runs.
pub const fn running(n: u32) -> u32 {
    1 << n
}
/// Timer n stops when it expires.
pub const fn one_shot(n: u32) -> u32 {
    1 << (4 + n)
}
/// Timer n counts at clock / 16.
pub const fn divided(n: u32) -> u32 {
    1 << (8 + n)
}
/// The timestamp counts.
pub const TIMESTAMP_ON: u32 = 1 << 12;
/// Timer 0 resets the chip when it expires.
pub const RESET_ON_EXPIRY: u32 = 1 << 31;

/// The timer clock, in ticks per microsecond: Nokia's kernel registers the timestamp as an 80 MHz clocksource, KnOS
/// sizes its delays as clock × us / 1e6.
pub const TICKS_PER_US: u32 = 80;

pub struct Timers<'a, M: Mmio + ?Sized>(&'a mut M);

pub fn timers<M: Mmio + ?Sized>(m: &mut M) -> Timers<'_, M> {
    Timers(m)
}

impl<M: Mmio + ?Sized> Timers<'_, M> {
    /// The timestamp from 0. Bare metal only: under Linux it is the kernel's clocksource.
    pub fn start_timestamp(&mut self) {
        self.0.w32(BASE + TIMESTAMP, 0);
        self.0.w32(BASE + RUN, TIMESTAMP_ON);
    }

    /// The timestamp: 80 ticks a microsecond, wrapping every 53.7 s.
    pub fn now(&mut self) -> u32 {
        self.0.r32(BASE + TIMESTAMP)
    }

    /// Microseconds since a [`Timers::now`] reading (up to the wrap).
    pub fn us_since(&mut self, t0: u32) -> u32 {
        self.now().wrapping_sub(t0) / TICKS_PER_US
    }

    /// The configuration word (which timers run, and how).
    pub fn config(&mut self) -> u32 {
        self.0.r32(BASE + RUN)
    }

    /// Restart the chip now: timer 0 as a watchdog that expires at once (the stock app's sequence; the threshold is
    /// what it clamps to).
    pub fn reset_now(&mut self) -> ! {
        self.0.w32(BASE + STOP, RESET_ON_EXPIRY | running(0));
        self.0.w32(BASE + COUNT, 0);
        self.0.w32(BASE + THRESHOLD, 0xFF);
        self.0.w32(BASE + RUN, RESET_ON_EXPIRY | running(0));
        loop {
            core::hint::spin_loop();
        }
    }
}
