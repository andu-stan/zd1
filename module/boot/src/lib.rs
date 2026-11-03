//! Bare-metal pieces shared by the two DM870 images (src/bin/): entry point, the board (physical = virtual, MMU off
//! as the preloader leaves it), console (UART1), panic handler.
#![no_std]

use core::panic::PanicInfo;
use dm870::clocks::{Gates, tgen};
use dm870::pads::{self, pads};
use dm870::timers::timers;
use dm870::uart::{UART1, uart};

pub use zd1_link::Link;

/// `_start`: stack, zero .bss, then `rust_main` (the binary's `main`, see [`entry!`]). The linker scripts put
/// .text.boot first and define _stack_top, _bss_start, _bss_end.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        core::arch::global_asm!(
            ".section .text.boot",
            ".global _start",
            "_start:",
            "    ldr  sp, =_stack_top",
            "    ldr  r0, =_bss_start",
            "    ldr  r1, =_bss_end",
            "    mov  r2, #0",
            "0:  cmp  r0, r1",
            "    strlo r2, [r0], #4",
            "    blo  0b",
            "    bl   rust_main",
            "1:  b    1b",
        );
        #[unsafe(no_mangle)]
        pub extern "C" fn rust_main() -> ! {
            $main()
        }
    };
}

/// The DM870 with the MMU off, as the preloader hands it over: registers at their physical addresses.
pub use dm870::Physical as Board;

/// What both images start with: the timestamp running, the console at 115200 (for the clock the preloader left), the
/// pads routed and the SPI slave's clock on, the link up.
pub fn link_up() -> Link<Board> {
    let mut board = Board;
    timers(&mut board).start_timestamp();
    uart(&mut board, UART1).set_baud(115_200);
    pads(&mut board).route(&pads::BL2);
    tgen(&mut board).enable(Gates::SPI_SLAVE);
    Link::new(board)
}

/// Console text to UART1, the module console (our MCU firmware logs it: `zd1 log`).
pub fn say(link: &mut Link<Board>, s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            uart(link.mmio(), UART1).put(b'\r');
        }
        uart(link.mmio(), UART1).put(b);
    }
}

pub fn say_u32(link: &mut Link<Board>, v: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    let mut v = v;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    say(link, core::str::from_utf8(&buf[i..]).unwrap());
}

pub fn say_hex(link: &mut Link<Board>, v: u32) {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut buf = *b"0x00000000";
    for i in 0..8 {
        buf[9 - i] = H[(v >> (4 * i)) as usize & 15];
    }
    say(link, core::str::from_utf8(&buf).unwrap());
}

/// Stay reachable: serve the link until the MCU asks for BL2 (HCR 0x21), then reboot into it.
pub fn serve_until_bl2(link: &mut Link<Board>, who: &str) -> ! {
    loop {
        link.poll();
        if link.bl2_requested() {
            reboot_to_bl2(link, who);
        }
    }
}

pub fn reboot_to_bl2(link: &mut Link<Board>, who: &str) -> ! {
    say(link, who);
    say(link, " HCR 0x21: rebooting into BL2\n");
    link.reboot_to_bl2()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let put = |s: &str| {
        for b in s.bytes() {
            uart(&mut Board, UART1).put(b);
        }
    };
    put("\r\n[zd1] PANIC");
    if let Some(l) = info.location() {
        put(" at ");
        put(l.file());
    }
    put("\r\n");
    loop {}
}
