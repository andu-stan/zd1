//! Boot stub for a Linux kernel on the module (image layout: module/os/mkpayload.py; linked at 0x61F00000,
//! linuxboot.ld).
//! The app image entered at 0x60008000 is [trampoline | header @0x40 | this stub | kernel Image | initramfs]; the
//! trampoline has copied us to 0x61F00000. We:
//!   1. bring up the MCU link (stock SPI register protocol, module/link); our text goes to UART1, the console.
//!      Status BOOTING (0x2000), not READY: the stock MCU takes READY as "start the setup task", which then ran into
//!      the silent kernel boot and never finished after a module restart (Wi-Fi source stuck, LED breathing red;
//!      dev/stock-os). To it BOOTING is "not usable yet, keep polling"; zd1-linkd says READY once Linux is up. BL2 is
//!      reached through BL1's serial menu (`zd1 module esc`) whatever happens here;
//!   2. with the slots flag (header +0x2C bit 0: the module OS's image, docs/module/storage.md): ask the MCU what to
//!      start. We say "zd1-boot" on reg 0x25 and wait up to 5 s for its answer on reg 0xC4 (b0: slot A, slot B or the
//!      rescue, b1: the good slot; our MCU firmware, mcu/src/modboot.c). A slot is the root file system (JFFS2 on its
//!      MTD partition, read-only: the initramfs is left out), the rescue is the initramfs. No answer (the MCU's
//!      bootloader relaying an image, the stock MCU firmware): the rescue. Without the MCU we don't know which slot is
//!      good, and a slot may hold an old system or none (an empty one panics, and the module restarts into it again);
//!      the rescue is always there, and its zd1-linkd takes the bootloader's HCR 0x21 into BL2. Without the flag the
//!      initramfs is the system (the emulator's dev boot);
//!   3. move the initramfs to header +0x28 (0: INITRD_AT) and the kernel to 0x60008000 (stored as raw deflate when
//!      header +0x30 gives its inflated length: copied just below this stub, inflated into place, the time printed),
//!      write ATAGs, jump (r0 = 0, r1 = machine, r2 = ATAGs, SVC, IRQs off, MMU and D-cache off as the preloader left
//!      them; the I-cache on: we switch it on at the start, the boot protocol allows either).

#![no_std]
#![no_main]

use core::ptr::{copy, copy_nonoverlapping, read_volatile, write_volatile};
use dm870::timers::timers;
use zd1_boot::{say, say_hex, say_u32};
use zd1_link::spi::spec::{boot, reg};

zd1_boot::entry!(main);

const HDR: usize = 0x6000_8040; // the preloader always enters the app image at 0x60008000
const MAGIC: u32 = u32::from_le_bytes(*b"ZLB1");
const KERNEL_AT: usize = 0x6000_8000; // the Image is linked for PHYS_OFFSET 0x60000000 + 0x8000
const ATAGS_AT: usize = 0x6000_0100;
const STUB_AT: usize = dm870::memory::BOOT_STUB as usize; // where the trampoline put us (linuxboot.ld)
const INITRD_AT: usize = 0x6180_0000; // default (header +0x28 = 0): below this stub, above the kernel's working set
const SLOTS: u32 = 1; // header +0x2C: ask the MCU for a system slot
const ANSWER_US: u32 = 5_000_000;
/// The slots' MTD partitions (module/os/os.toml mtdparts: mtd1 system-a, mtd2 system-b).
const ROOT_A: &[u8] = b" root=/dev/mtdblock1 rootfstype=jffs2 ro zd1.slot=a";
const ROOT_B: &[u8] = b" root=/dev/mtdblock2 rootfstype=jffs2 ro zd1.slot=b";

fn hdr(off: usize) -> u32 {
    unsafe { read_volatile((HDR + off) as *const u32) }
}
fn w32(a: usize, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}
fn r32(a: usize) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}

/// Nokia's 2.6.24 mach-sulu kernel (blobs/module/nokia-hd1) spins in its idle loop at 240 MIPS: default_idle disables
/// IRQs, loads TIF_NEED_RESCHED, ignores it and returns (arch_idle is empty). With that build's exact words in place
/// it becomes mainline's: `tst r3, #2; bleq cpu_arm926_do_idle` (WFI, which a masked IRQ ends), IRQs on again. The
/// emulator applies the same patch (emu/renode/cr870/CR870.cs, PatchLinuxIdle). Physical addresses (VA - 0x60000000).
fn idle_wfi(link: &mut zd1_link::Link<zd1_boot::Board>) {
    const WORDS: [(usize, u32); 6] = [
        (0x6002_75a8, 0xe10f_3000),
        (0x6002_75ac, 0xe3c3_3080),
        (0x6002_75b0, 0xe121_f003),
        (0x6002_75a4, 0xe593_3000),
        (0x6002_7598, 0xe121_f003),
        (0x6002_f020, 0xee07_0f90),
    ];
    if WORDS.iter().all(|&(a, w)| r32(a) == w) {
        w32(0x6002_75a8, 0xe313_0002); // tst r3, #2 (TIF_NEED_RESCHED)
        w32(0x6002_75ac, 0x0b00_1e93); // bleq cpu_arm926_do_idle (0xc002f000)
        w32(0x6002_75b0, 0xe321_f013); // msr CPSR_c, #0x13: SVC, IRQ + FIQ on
        say(link, "[linuxboot] idle loop: WFI\n");
    } else {
        say(link, "[linuxboot] idle loop: not the kernel we know, left as it is\n");
    }
}

/// The first-level page table for the inflate (16 KB, 16 KB aligned): below the kernel, where its own goes later.
const TTB_AT: usize = 0x6000_4000;

/// The MMU on with a flat map (every 1 MB section to itself), the 32 MB of SDRAM write-back cached and everything else
/// (the peripherals) not, and the D-cache on: the ARM926 caches data only through the MMU, and the inflate with the
/// D-cache off took 2.5 s. Addresses don't change, so the code runs on as it was.
fn caches_on() {
    for i in 0..4096usize {
        let sdram = (0x600..0x620).contains(&i);
        // section: base, AP = 3 (read/write), domain 0, bit 4 set (ARMv5), C + B (write-back) for SDRAM
        w32(TTB_AT + 4 * i, (i << 20) as u32 | 3 << 10 | 1 << 4 | if sdram { 0b1100 } else { 0 } | 0b10);
    }
    unsafe {
        core::arch::asm!(
            "mov r0, #0",
            "mcr p15, 0, r0, c7, c10, 4",        // drain the write buffer: the table is in memory
            "mcr p15, 0, r1, c2, c0, 0",         // the table
            "mov r0, #3",
            "mcr p15, 0, r0, c3, c0, 0",         // domain 0: manager (no permission checks)
            "mov r0, #0",
            "mcr p15, 0, r0, c8, c7, 0",         // the TLBs invalidated
            "mcr p15, 0, r0, c7, c6, 0",         // the D-cache invalidated
            "mrc p15, 0, r0, c1, c0, 0",
            "orr r0, r0, #0x5",                  // M (MMU) + C (D-cache)
            "mcr p15, 0, r0, c1, c0, 0",
            in("r1") TTB_AT, out("r0") _);
    }
}

/// Back as the preloader left it for the kernel: the D-cache cleaned into memory and invalidated, the MMU and the
/// D-cache off, the TLBs and the I-cache invalidated.
fn caches_off() {
    unsafe {
        core::arch::asm!(
            "2: mrc p15, 0, APSR_nzcv, c7, c14, 3",   // test, clean and invalidate the D-cache, until it is clean
            "bne 2b",
            "mov r0, #0",
            "mcr p15, 0, r0, c7, c10, 4",        // drain the write buffer
            "mrc p15, 0, r0, c1, c0, 0",
            "bic r0, r0, #0x5",
            "mcr p15, 0, r0, c1, c0, 0",
            "mov r0, #0",
            "mcr p15, 0, r0, c8, c7, 0",
            "mcr p15, 0, r0, c7, c5, 0",
            out("r0") _);
    }
}

/// The inflater's state (~11 KB): here rather than on the 64 KB stack.
static mut INFLATE: core::mem::MaybeUninit<miniz_oxide::inflate::core::DecompressorOxide> =
    core::mem::MaybeUninit::uninit();

/// The kernel's deflate stream (at `src`, `n` bytes) into its place, `len` bytes. False if it doesn't come out whole.
fn inflate(src: usize, n: usize, len: usize) -> bool {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};
    let input = unsafe { core::slice::from_raw_parts(src as *const u8, n) };
    let output = unsafe { core::slice::from_raw_parts_mut(KERNEL_AT as *mut u8, len) };
    let state = unsafe { (*core::ptr::addr_of_mut!(INFLATE)).write(DecompressorOxide::new()) };
    let (st, _, out) = decompress(state, input, output, 0, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
    st == TINFLStatus::Done && out == len
}

fn main() -> ! {
    unsafe {
        // the I-cache (allowed with the MMU off): ~10x on code from SDRAM
        core::arch::asm!("mov r0, #0", "mcr p15, 0, r0, c7, c5, 0", "mrc p15, 0, r0, c1, c0, 0",
                         "orr r0, r0, #0x1000", "mcr p15, 0, r0, c1, c0, 0", out("r0") _);
    }
    let mut link = zd1_boot::link_up();
    link.set_value(reg::VERSION_IMAGE, b"zd1-boot\0");
    link.set_status(zd1_link::spi::spec::status::BOOTING);
    say(&mut link, "\n[linuxboot] zd1 Linux boot stub\n");
    if hdr(0) != MAGIC {
        say(&mut link, "[linuxboot] bad header magic: serving the link (HCR 0x21 -> BL2)\n");
        zd1_boot::serve_until_bl2(&mut link, "[linuxboot]");
    }
    // Everything we need from the header, before the kernel copy overwrites it.
    let (k_off, k_len) = (hdr(0x10) as usize, hdr(0x14) as usize);
    let (i_off, i_len) = (hdr(0x18) as usize, hdr(0x1C) as usize);
    let mach = hdr(0x24); // 0x20: reserved
    let initrd_at = match hdr(0x28) {
        0 => INITRD_AT,
        a => a as usize,
    }; // a big (uncompressed) one goes lower
    let flags = hdr(0x2C);
    let inflated = hdr(0x30) as usize; // 0: the kernel is stored as it is
    let mut cmdline = [0u8; 512];
    let mut clen = 0;
    while clen < 383 {
        let b = unsafe { read_volatile((HDR + 0x40 + clen) as *const u8) };
        if b == 0 {
            break;
        }
        cmdline[clen] = b;
        clen += 1;
    }
    let base = KERNEL_AT;
    let mut i_len = i_len;
    if flags & SLOTS != 0 {
        let t0 = timers(link.mmio()).now();
        while link.reg(reg::BOOT) == 0 && timers(link.mmio()).us_since(t0) < ANSWER_US {
            link.poll();
        }
        let answer = link.reg(reg::BOOT);
        let (start, good) = ((answer >> 8) as u8, answer as u8);
        let mut add = |s: &[u8]| {
            for &b in s {
                if clen < cmdline.len() - 1 {
                    cmdline[clen] = b;
                    clen += 1;
                }
            }
        };
        add(if good == boot::SLOT_B { b" zd1.good=b" } else { b" zd1.good=a" });
        match start {
            boot::RESCUE => {
                say(&mut link, "[linuxboot] the rescue (the MCU's choice)\n");
                add(b" zd1.rescue");
            }
            boot::SLOT_B => {
                say(&mut link, "[linuxboot] slot B (the MCU's choice)\n");
                add(ROOT_B);
                i_len = 0;
            }
            boot::SLOT_A => {
                say(&mut link, "[linuxboot] slot A (the MCU's choice)\n");
                add(ROOT_A);
                i_len = 0;
            }
            _ => {
                say(&mut link, "[linuxboot] no answer from the MCU: the rescue\n");
                add(b" zd1.rescue");
            }
        }
    }
    cmdline[clen] = 0;

    say(&mut link, "[linuxboot] kernel ");
    say_hex(&mut link, k_len as u32);
    say(&mut link, " B, initramfs ");
    say_hex(&mut link, i_len as u32);
    say(&mut link, " B\n");

    // where they go must hold them: the initramfs below this stub, a kernel stored as it is below the initramfs
    let k_end = KERNEL_AT + if inflated == 0 { k_len } else { 0 };
    if (i_len > 0 && (initrd_at + i_len > STUB_AT || k_end > initrd_at)) || k_end > STUB_AT {
        say(&mut link, "[linuxboot] no room for the kernel and the initramfs: serving the link (HCR 0x21 -> BL2)\n");
        zd1_boot::serve_until_bl2(&mut link, "[linuxboot]");
    }
    say(&mut link, "[linuxboot] booting Linux\n");
    link.release_request_line();
    unsafe {
        if i_len > 0 {
            copy_nonoverlapping((base + i_off) as *const u8, initrd_at as *mut u8, i_len);
        }
        if inflated == 0 {
            copy((base + k_off) as *const u8, KERNEL_AT as *mut u8, k_len);
        } // overlapping: memmove
    }
    if inflated > 0 {
        // out of the way of its own output: just below this stub (above the initramfs, below us)
        let at = (STUB_AT - k_len) & !0xFFF;
        if (i_len > 0 && initrd_at + i_len > at) || KERNEL_AT + inflated > at {
            say(&mut link, "[linuxboot] no room to inflate the kernel: serving the link (HCR 0x21 -> BL2)\n");
            zd1_boot::serve_until_bl2(&mut link, "[linuxboot]");
        }
        let t0 = timers(link.mmio()).now();
        caches_on();
        unsafe {
            copy_nonoverlapping((base + k_off) as *const u8, at as *mut u8, k_len);
        }
        let ok = inflate(at, k_len, inflated);
        caches_off();
        let us = timers(link.mmio()).us_since(t0);
        if !ok {
            say(&mut link, "[linuxboot] the kernel doesn't inflate: serving the link (HCR 0x21 -> BL2)\n");
            zd1_boot::serve_until_bl2(&mut link, "[linuxboot]");
        }
        say(&mut link, "[linuxboot] kernel inflated: ");
        say_u32(&mut link, inflated as u32);
        say(&mut link, " B in ");
        say_u32(&mut link, us / 1000);
        say(&mut link, " ms\n");
    }
    idle_wfi(&mut link);
    // ATAG list: CORE, MEM 32 MB @ 0x60000000, INITRD2, CMDLINE, NONE.
    let mut p = ATAGS_AT;
    let mut tag = |words: &[u32]| {
        for &w in words {
            w32(p, w);
            p += 4;
        }
    };
    tag(&[5, 0x5441_0001, 0, 4096, 0]);
    tag(&[4, 0x5441_0002, 32 << 20, 0x6000_0000]);
    if i_len > 0 {
        tag(&[4, 0x5442_0005, initrd_at as u32, i_len as u32]);
    }
    let clen = clen + 1;
    let cwords = clen.div_ceil(4);
    tag(&[2 + cwords as u32, 0x5441_0009]);
    for i in 0..cwords {
        let mut w = 0u32;
        for j in 0..4 {
            let k = i * 4 + j;
            if k < clen {
                w |= (cmdline[k] as u32) << (8 * j);
            }
        }
        tag(&[w]);
    }
    tag(&[0, 0]);
    unsafe {
        core::arch::asm!(
            "msr cpsr_c, #0xd3",                 // SVC, IRQ + FIQ off
            "mov r0, #0",
            "mcr p15, 0, r0, c7, c5, 0",         // the I-cache invalidated: the kernel's text was written (idle_wfi)
            "bx r3",
            in("r3") KERNEL_AT, in("r1") mach, in("r2") ATAGS_AT, options(noreturn));
    }
}
