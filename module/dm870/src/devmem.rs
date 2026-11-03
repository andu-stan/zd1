//! /dev/mem as a bus: each 4 KB page mapped the first time it is touched, kept until the end; and regions mapped whole,
//! for memory shared with another core.

use crate::{Mmio, clocks, gpio, mailbox, pads, spi_slave, timers, uart};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

const PAGE: u32 = 0x1000;

/// Every block this crate names, a page each: mapped by [`DevMem::open`].
const BLOCKS: [u32; 8] =
    [clocks::BASE, gpio::BASE, timers::BASE, pads::BASE, spi_slave::BASE, mailbox::BASE, uart::UART0, uart::UART1];

/// Physical memory through /dev/mem. The register blocks this crate names are mapped when it opens, so a kernel that
/// refuses shows there, as an error, and the [`Mmio`] accesses to them can't fail. Other addresses: [`DevMem::read32`]
/// and [`DevMem::write32`], which map their page on the way and say when that fails. Through [`Mmio`] an address
/// outside the blocks maps its page too, and a refusal there panics: the bus has no way to fail.
pub struct DevMem {
    file: File,
    pages: HashMap<u32, usize>,
}

impl DevMem {
    pub fn open() -> io::Result<DevMem> {
        let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_SYNC).open("/dev/mem")?;
        let mut m = DevMem { file, pages: HashMap::new() };
        for b in BLOCKS {
            m.map(b)?;
        }
        Ok(m)
    }

    /// Map a page ahead of time, to find out now rather than at the first access whether the kernel lets us.
    pub fn map(&mut self, pa: u32) -> io::Result<()> {
        self.at(pa).map(|_| ())
    }

    pub fn read32(&mut self, pa: u32) -> io::Result<u32> {
        Ok(unsafe { std::ptr::read_volatile(self.at(pa)? as *const u32) })
    }

    pub fn write32(&mut self, pa: u32, v: u32) -> io::Result<()> {
        unsafe { std::ptr::write_volatile(self.at(pa)? as *mut u32, v) };
        Ok(())
    }

    /// Where `pa` is mapped, its page mapped first if it isn't yet.
    fn at(&mut self, pa: u32) -> io::Result<usize> {
        let page = pa & !(PAGE - 1);
        let va = match self.pages.get(&page) {
            Some(&va) => va,
            None => {
                let va = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        PAGE as usize,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        self.file.as_raw_fd(),
                        page as libc::off_t,
                    )
                };
                if va == libc::MAP_FAILED {
                    return Err(io::Error::other(format!("/dev/mem: 0x{pa:08X}: {}", io::Error::last_os_error())));
                }
                self.pages.insert(page, va as usize);
                va as usize
            }
        };
        Ok(va + (pa & (PAGE - 1)) as usize)
    }

    /// `len` bytes from `pa` (page aligned) in one mapping, for memory shared with another bus master (Warp0's
    /// window and local RAM): bulk copies and single words, without a page lookup each.
    pub fn region(&self, pa: u32, len: usize) -> io::Result<Region> {
        let va = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.file.as_raw_fd(),
                pa as libc::off_t,
            )
        };
        if va == libc::MAP_FAILED {
            return Err(io::Error::other(format!("/dev/mem: 0x{pa:08X} +0x{len:X}: {}", io::Error::last_os_error())));
        }
        Ok(Region { pa, va: va as *mut u8, len })
    }

    fn bus(&mut self, pa: u32) -> usize {
        self.at(pa).unwrap_or_else(|e| panic!("{e}"))
    }
}

impl Mmio for DevMem {
    fn r32(&mut self, pa: u32) -> u32 {
        unsafe { std::ptr::read_volatile(self.bus(pa) as *const u32) }
    }
    fn w32(&mut self, pa: u32, v: u32) {
        unsafe { std::ptr::write_volatile(self.bus(pa) as *mut u32, v) }
    }
    fn r8(&mut self, pa: u32) -> u8 {
        unsafe { std::ptr::read_volatile(self.bus(pa) as *const u8) }
    }
    fn w8(&mut self, pa: u32, v: u8) {
        unsafe { std::ptr::write_volatile(self.bus(pa) as *mut u8, v) }
    }
}

/// A contiguous mapping of physical memory ([`DevMem::region`]), uncached (/dev/mem opens with O_SYNC), unmapped when
/// dropped. Addresses are physical; every access is checked against the region.
pub struct Region {
    pa: u32,
    va: *mut u8,
    len: usize,
}

// The mapping belongs to the Region alone; moving it to another thread moves nothing else.
unsafe impl Send for Region {}

impl Region {
    fn at(&self, pa: u32, len: usize) -> *mut u8 {
        let off = pa.wrapping_sub(self.pa) as usize;
        assert!(pa >= self.pa && off.checked_add(len).is_some_and(|e| e <= self.len), "0x{pa:08X} +{len}: outside");
        unsafe { self.va.add(off) }
    }

    pub fn read32(&self, pa: u32) -> u32 {
        unsafe { std::ptr::read_volatile(self.at(pa, 4) as *const u32) }
    }

    pub fn write32(&self, pa: u32, v: u32) {
        unsafe { std::ptr::write_volatile(self.at(pa, 4) as *mut u32, v) }
    }

    /// A copy in: memcpy, whose multi-word bursts (LDM/STM) write uncached memory at ~80-130 MB/s, where one word at
    /// a time manages ~9 MB/s [HW 2026-10-24]. The fence keeps the compiler from moving the copy past a store that
    /// hands the data over (the mapping is strongly ordered: the hardware keeps the order).
    pub fn put(&self, pa: u32, data: &[u8]) {
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.at(pa, data.len()), data.len()) };
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }

    /// A copy out (~57-88 MB/s), after the load that said the data is there.
    pub fn get(&self, pa: u32, out: &mut [u8]) {
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        unsafe { std::ptr::copy_nonoverlapping(self.at(pa, out.len()), out.as_mut_ptr(), out.len()) };
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.va as *mut libc::c_void, self.len) };
    }
}
