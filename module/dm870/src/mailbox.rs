//! The BL2 mailbox (0xB1005000): three words BL1 reads at a reset, kept over a warm reset, lost at a power cycle [HW].
//! `{MAGIC, mode, !MAGIC}` makes BL1 start BL2 instead of the app (docs/module/boot-chain.md).

use crate::Mmio;

pub const BASE: u32 = 0xB100_5000;
pub const MAGIC: u32 = 0xDEAD_BEEF;
/// The mode the stock app asks for (BL2's normal start).
pub const MODE_BL2: u32 = 0;

/// Leave the request; the next reset starts BL2.
pub fn request_bl2(m: &mut (impl Mmio + ?Sized)) {
    m.w32(BASE, MAGIC);
    m.w32(BASE + 4, MODE_BL2);
    m.w32(BASE + 8, !MAGIC);
}
