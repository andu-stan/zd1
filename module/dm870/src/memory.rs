//! Memory: where things are, and the regions our OS keeps for itself (docs/module/memory-map.md, Memory;
//! docs/module/warp.md section 4).

/// The on-chip SRAM ("UMR0", 64 KB); it also appears every 64 KB through 0x003FFFFF [HW, JTAG 2026-10-25].
pub const SRAM: u32 = 0x0040_0000;
pub const SRAM_LEN: u32 = 64 << 10;
/// The SRAM uncached (the Ethernet MAC's DMA uses this alias).
pub const SRAM_UNCACHED: u32 = 0x0050_0000;

/// The Wi-Fi core's SRAM window (128 KB, repeated through 2 MB) and its registers.
pub const WILD_SRAM: u32 = 0x3000_0000;
pub const WILD_REGS: u32 = 0x3020_0000;

/// SDRAM, 32 MB; images run at 0x60008000.
pub const SDRAM: u32 = 0x6000_0000;
pub const SDRAM_LEN: u32 = 32 << 20;
pub const SDRAM_UNCACHED: u32 = 0x7000_0000;

/// The top megabyte, which Linux doesn't get (`mem=31M`): the boot stub, then Warp1's program window.
pub const BOOT_STUB: u32 = 0x61F0_0000;
/// Warp1's program window (`wsl` maps it; a MIPS image linked here runs there): 256 KB.
pub const WARP1_WINDOW: u32 = 0x61FC_0000;
pub const WARP1_WINDOW_LEN: u32 = 256 << 10;
