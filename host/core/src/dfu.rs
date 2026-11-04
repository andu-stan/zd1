//! The B&W DFU bootloader of the main board's MCU (03EB:2FF8), reverse-engineered byte for byte from the stock
//! updater (`re/ghidra/setup.py mcu-updater`; docs/mcu): dfu-programmer's Atmel/FLIP protocol plus B&W's
//! deviations. GETSTATUS returns 6 bytes, the 6th (normally iString) the COPROCESSOR status; end-of-transfer
//! selectors 07/08/09 and `06 09 t1 t2` start the network module's download; `04 04 00 00 00` starts BL2 next to
//! `04 03 01 00 00` (the app). The relay: the bootloader takes a module image in 1 KB blocks and hands it to BL2 as
//! XModem.
//!
//! Requests (class, to interface 0): DNLOAD 0x21/1 and UPLOAD 0xA1/2 with wValue = a transaction counter,
//! GETSTATUS 0xA1/3 (6 bytes), CLRSTATUS 0x21/4, GETSTATE 0xA1/5, ABORT 0x21/6.
//!
//! Nothing here writes fuses or the bootloader's region (below 0x4000; BOOTPROT protects it as well).

use crate::backup::Backup;
use crate::image::{BOOTLOADER_END, FLASH_SIZE, ISP_WORD, USER_PAGE_SIZE};
use crate::usb::{Setup, Usb};
use crate::{Error, Events, Result};

pub const VID: u16 = 0x03EB;
pub const PID: u16 = 0x2FF8;
pub const IFACE: u8 = 0;
/// The 64 KB addressing page, and the largest program / read chunk.
pub const PAGE: u32 = 0x1_0000;
pub const BLOCK: u32 = 0x400;

/// Memory units (atmel_select_memory).
pub const MEM_FLASH: u8 = 0x00;
pub const MEM_USER: u8 = 0x06;
/// The network module's application section, and its bootloader (BL2) section: the relay's targets.
pub const MEM_COPROC: u8 = 0x07;
pub const MEM_COPROC_BOOT: u8 = 0x08;

/// The coprocessor status (GETSTATUS byte 5), from the stock updater's table.
pub fn coproc_status_name(s: u8) -> &'static str {
    match s {
        1 => "power-off",
        2 => "power-on",
        3 => "ready",
        4 => "err-start-app",
        5 => "timeout-start-app",
        0x11 => "starting-BL2",
        0x12 => "BL2-ready",
        0x13 => "download-active",
        0x14 => "err-start-BL2",
        0x15 => "timeout-start-BL2",
        _ => "?",
    }
}

/// The command frames: each the DNLOAD payload, checked against the decompiled stock updater (tests).
pub mod frame {
    use super::BLOCK;
    pub fn select_ena(unit: u8) -> Vec<u8> {
        vec![0x06, 0x06, unit, 0x00]
    }
    pub fn select_memory(mem: u8) -> Vec<u8> {
        vec![0x06, 0x03, 0x00, mem]
    }
    pub fn select_page(page: u16) -> Vec<u8> {
        vec![0x06, 0x03, 0x01, (page >> 8) as u8, page as u8]
    }
    pub fn erase_chip() -> Vec<u8> {
        vec![0x04, 0x00, 0xFF]
    }
    pub fn start_app() -> Vec<u8> {
        vec![0x04, 0x03, 0x01, 0x00, 0x00]
    }
    pub fn start_bl2() -> Vec<u8> {
        vec![0x04, 0x04, 0x00, 0x00, 0x00]
    }
    pub fn read_mem(s: u16, e: u16) -> Vec<u8> {
        vec![0x03, 0x00, (s >> 8) as u8, s as u8, (e >> 8) as u8, e as u8]
    }
    /// End of a relay: 7 the module's app section, 8 its BL2 section, 9 extra.
    pub fn eot(kind: u8) -> Vec<u8> {
        vec![0x06, 0x04, kind, 0x00]
    }
    pub fn coproc_init(t1: u16, t2: u16) -> Vec<u8> {
        vec![0x06, 0x09, (t1 >> 8) as u8, t1 as u8, (t2 >> 8) as u8, t2 as u8]
    }

    /// The Atmel/DFU file suffix after each program block (its CRC left zero).
    pub fn suffix() -> [u8; 16] {
        [0, 0, 0, 0, 0x10, 0x44, 0x46, 0x55, 0x01, 0x10, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    }

    /// Write `data` (<= 1 KB) at `within` (the low 16 bits in the selected page): atmel_flash_block for a UC3, the
    /// header at 0..5, the data at (within & 0x3F) + 0x40, then the suffix.
    pub fn program_block(within: u16, data: &[u8]) -> Vec<u8> {
        assert!(data.len() <= BLOCK as usize && !data.is_empty());
        let off = (within as usize & 0x3F) + 0x40;
        let end = within as usize + data.len() - 1;
        let mut b = vec![0u8; off + data.len() + 16];
        b[..6].copy_from_slice(&[0x01, 0x00, (within >> 8) as u8, within as u8, (end >> 8) as u8, end as u8]);
        b[off..off + data.len()].copy_from_slice(data);
        b[off + data.len()..].copy_from_slice(&suffix());
        b
    }

    /// One relay block (padded to 1 KB): atmel_flash_coproc_block, the header `01 00 00 00 03 FF`, the data at
    /// 0x40, the suffix. The bootloader strips this and re-frames the 1 KB as XModem to the module.
    pub fn coproc_block(data: &[u8]) -> Vec<u8> {
        assert!(data.len() <= BLOCK as usize);
        let mut b = vec![0u8; 0x40 + BLOCK as usize + 16];
        b[..6].copy_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x03, 0xFF]);
        b[0x40..0x40 + data.len()].copy_from_slice(data);
        b[0x40 + BLOCK as usize..].copy_from_slice(&suffix());
        b
    }
}

/// GETSTATUS.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub status: u8,
    pub poll_ms: u32,
    pub state: u8,
    pub coproc: u8,
}

/// The DFU bootloader, interface 0 claimed.
pub struct Dfu<U: Usb> {
    pub usb: U,
    transaction: u16,
}

const TIMEOUT: u32 = 10_000;

impl<U: Usb> Dfu<U> {
    pub fn new(usb: U) -> Dfu<U> {
        Dfu { usb, transaction: 0 }
    }

    fn setup(&self, bm: u8, request: u8, value: u16) -> Setup {
        Setup { bm, request, value, index: IFACE as u16 }
    }

    async fn download(&mut self, payload: &[u8]) -> Result<()> {
        let s = self.setup(0x21, 1, self.transaction);
        self.transaction = self.transaction.wrapping_add(1);
        self.usb.control_out(s, payload, TIMEOUT).await.map_err(|e| Error::Usb("DFU download", e))
    }

    async fn upload(&mut self, len: u16) -> Result<Vec<u8>> {
        let s = self.setup(0xA1, 2, self.transaction);
        self.transaction = self.transaction.wrapping_add(1);
        self.usb.control_in(s, len, TIMEOUT).await.map_err(|e| Error::Usb("DFU upload", e))
    }

    pub async fn status(&self) -> Result<Status> {
        let r = self
            .usb
            .control_in(self.setup(0xA1, 3, 0), 6, TIMEOUT)
            .await
            .map_err(|e| Error::Usb("DFU GETSTATUS", e))?;
        if r.len() != 6 {
            return Err(Error::Device("DFU: short GETSTATUS".into()));
        }
        Ok(Status { status: r[0], poll_ms: u32::from_le_bytes([r[1], r[2], r[3], 0]), state: r[4], coproc: r[5] })
    }

    async fn clear_status(&self) -> Result<()> {
        self.usb.control_out(self.setup(0x21, 4, 0), &[], TIMEOUT).await.map_err(|e| Error::Usb("DFU CLRSTATUS", e))
    }

    async fn state(&self) -> Result<u8> {
        let r =
            self.usb.control_in(self.setup(0xA1, 5, 0), 1, TIMEOUT).await.map_err(|e| Error::Usb("DFU GETSTATE", e))?;
        r.first().copied().ok_or_else(|| Error::Device("DFU: no state".into()))
    }

    async fn abort(&self) -> Result<()> {
        self.usb.control_out(self.setup(0x21, 6, 0), &[], TIMEOUT).await.map_err(|e| Error::Usb("DFU ABORT", e))
    }

    /// Into dfuIDLE: an idle download or upload aborted, an error cleared.
    pub async fn make_idle(&self) -> Result<()> {
        for _ in 0..4 {
            match self.state().await? {
                2 => return Ok(()),              // dfuIDLE
                5 | 9 => self.abort().await?,    // dfuDNLOAD-IDLE, dfuUPLOAD-IDLE
                _ => self.clear_status().await?, // dfuERROR and the rest
            }
        }
        Err(Error::Device("DFU: could not reach dfuIDLE".into()))
    }

    async fn command(&mut self, payload: &[u8], check: bool) -> Result<()> {
        self.make_idle().await?;
        self.download(payload).await?;
        if check {
            let s = self.status().await?;
            if s.status != 0 {
                return Err(Error::Device(format!(
                    "DFU: status 0x{:02x} after {}",
                    s.status,
                    crate::hex(&payload[..2])
                )));
            }
        }
        Ok(())
    }

    async fn select_ena(&mut self) -> Result<()> {
        self.command(&frame::select_ena(1), false).await
    }

    pub async fn select_memory(&mut self, mem: u8) -> Result<()> {
        self.select_ena().await?;
        self.command(&frame::select_memory(mem), false).await
    }

    async fn select_page(&mut self, page: u16) -> Result<()> {
        self.select_ena().await?;
        self.command(&frame::select_page(page), false).await
    }

    /// Read flash offsets [start, end) of memory `mem`.
    pub async fn read(&mut self, start: u32, end: u32, mem: u8, ev: &dyn Events) -> Result<Vec<u8>> {
        self.select_memory(mem).await?;
        let mut out = Vec::with_capacity((end - start) as usize);
        let mut a = start;
        while a < end {
            self.select_page((a >> 16) as u16).await?;
            let within = a & 0xFFFF;
            let n = BLOCK.min(end - a).min(PAGE - within);
            self.make_idle().await?;
            self.download(&frame::read_mem(within as u16, (within + n - 1) as u16)).await?;
            let d = self.upload(n as u16).await?;
            if d.len() != n as usize {
                return Err(Error::Device(format!("DFU: short upload {}/{n}", d.len())));
            }
            out.extend(d);
            a += n;
            ev.progress("read", (a - start) as u64, (end - start) as u64);
        }
        Ok(out)
    }

    /// Erase the flash (the bootloader's region stays: BOOTPROT).
    pub async fn erase(&mut self) -> Result<()> {
        self.select_ena().await?;
        self.command(&frame::erase_chip(), false).await?;
        for _ in 0..400 {
            let s = self.status().await?;
            if s.status == 0 && (s.state == 2 || s.state == 5) {
                return Ok(());
            }
            self.usb.sleep(50).await;
        }
        Err(Error::Device("DFU: the erase did not complete".into()))
    }

    /// Program flash offsets [start, end) from `data` (its first byte at `start`); blocks of 0xFF are skipped (the
    /// erase left them so). Never below the bootloader's end, nor past the flash's.
    pub async fn program(&mut self, start: u32, data: &[u8], ev: &dyn Events) -> Result<()> {
        if start < BOOTLOADER_END {
            return Err(Error::Failed(format!(
                "refusing to write the bootloader's region (below 0x{BOOTLOADER_END:x})"
            )));
        }
        let end = start as usize + data.len();
        if end > FLASH_SIZE as usize {
            return Err(Error::Failed(format!(
                "refusing: 0x{start:x}..0x{end:x} ends past the flash (0x{FLASH_SIZE:x})"
            )));
        }
        let end = end as u32;
        self.select_memory(crate::dfu::MEM_FLASH).await?;
        let mut a = start;
        while a < end {
            self.select_page((a >> 16) as u16).await?;
            let within = a & 0xFFFF;
            let n = BLOCK.min(end - a).min(PAGE - within);
            let chunk = &data[(a - start) as usize..(a - start + n) as usize];
            if chunk.iter().any(|b| *b != 0xFF) {
                self.make_idle().await?;
                self.download(&frame::program_block(within as u16, chunk)).await?;
                let s = self.status().await?;
                if s.status != 0 {
                    return Err(Error::Device(format!("DFU: program status 0x{:02x} at 0x{a:x}", s.status)));
                }
            }
            a += n;
            ev.progress("write", (a - start) as u64, (end - start) as u64);
        }
        Ok(())
    }

    /// Program the user page (512 bytes) and read it back. It holds the ISP word at 0x1FC: anything but the known
    /// one makes the bootloader enter DFU at every cold boot, so another needs `force`.
    pub async fn write_user_page(&mut self, data: &[u8], force: bool, ev: &dyn Events) -> Result<()> {
        if data.len() != USER_PAGE_SIZE as usize {
            return Err(Error::Failed(format!("a user page is 512 bytes (this: {})", data.len())));
        }
        let word = u32::from_be_bytes(data[0x1FC..0x200].try_into().unwrap());
        if word != ISP_WORD && !force {
            return Err(Error::Failed(format!(
                "refusing: the ISP word 0x{word:08x} is not the known-good 0x{ISP_WORD:08x} \
                                              (the bootloader would enter DFU at every cold boot)"
            )));
        }
        self.select_memory(MEM_USER).await?;
        self.select_page(0).await?;
        for off in (0..USER_PAGE_SIZE).step_by(BLOCK as usize) {
            let end = (off + BLOCK).min(USER_PAGE_SIZE);
            self.make_idle().await?;
            self.download(&frame::program_block(off as u16, &data[off as usize..end as usize])).await?;
            let s = self.status().await?;
            if s.status != 0 {
                return Err(Error::Device(format!("DFU: user page status 0x{:02x}", s.status)));
            }
        }
        let back = self.read(0, USER_PAGE_SIZE, MEM_USER, ev).await?;
        if back != data {
            let i = back.iter().zip(data).position(|(a, b)| a != b).unwrap_or(0);
            return Err(Error::Failed(format!(
                "the user page reads back 0x{:02x} at 0x{i:x}, not 0x{:02x}",
                back[i], data[i]
            )));
        }
        Ok(())
    }

    /// Leave DFU: the app (or BL2). The bootloader jumps there on the trigger (a zero-length download), USB still
    /// attached: the device may vanish during that transfer, and the new firmware needs a moment before anything
    /// talks to it (a setup that arrives while it brings USB up wedged ours in the emulator).
    pub async fn start(&mut self, bl2: bool) -> Result<()> {
        self.select_ena().await?;
        self.command(&if bl2 { frame::start_bl2() } else { frame::start_app() }, false).await?;
        let s = self.setup(0x21, 1, self.transaction);
        let _ = self.usb.control_out(s, &[], 1000).await; // the device may be gone before it answers
        self.usb.sleep(500).await;
        Ok(())
    }

    /// The module's status as the bootloader sees it (GETSTATUS byte 5).
    pub async fn coproc_status(&self) -> Result<u8> {
        Ok(self.status().await?.coproc)
    }

    /// Relay a module image (a .bcd) through the bootloader into the module's app section (or its BL2 section: a
    /// protected region, the way back if the app section fails). The stock updater's sequence: coproc-init, wait for BL2-ready, the image in
    /// 1 KB blocks, the end of transfer. BL2 erases the app partition as the download starts: once streaming, a stop
    /// leaves it invalid until a complete image is installed. `max_blocks`: a measurement (stop after that many,
    /// no end of transfer); `handover_only`: stop at BL2-ready (the emulator's fast path).
    pub async fn relay(&mut self, image: &[u8], opts: &Relay, ev: &dyn Events) -> Result<()> {
        let t0 = crate::now_ms();
        ev.step(&format!("coproc-init (t1={} t2={}); waiting for the module's BL2", opts.t1, opts.t2));
        self.select_ena().await?;
        self.command(&frame::coproc_init(opts.t1, opts.t2), false).await?;
        let mut last = None;
        let mut ready = false;
        for s in 0..opts.wait_s {
            self.usb.sleep(1000).await;
            ev.wait("the module's BL2 starting", s as u64 + 1);
            let st = self.coproc_status().await?;
            if last != Some(st) {
                ev.step(&format!("module: 0x{st:02x} {}", coproc_status_name(st)));
                last = Some(st);
            }
            if st == 0x12 {
                ev.step(&format!("BL2 ready {:.1} s after coproc-init", (crate::now_ms() - t0) / 1000.0));
                ready = true;
                break;
            }
            if ![2, 3, 0x11].contains(&st) {
                return Err(Error::Device(format!(
                    "the module didn't start BL2: 0x{st:02x} {}",
                    coproc_status_name(st)
                )));
            }
        }
        if !ready {
            return Err(Error::Timeout("the module's BL2 didn't get ready".into()));
        }
        if opts.handover_only {
            return Ok(());
        }
        let total = image.len().div_ceil(BLOCK as usize);
        let n = opts.max_blocks.map_or(total, |m| m.min(total));
        let t1 = crate::now_ms();
        for (i, chunk) in image.chunks(BLOCK as usize).take(n).enumerate() {
            if i == 0 {
                self.select_ena().await?;
                self.command(&frame::select_memory(if opts.bl2 { MEM_COPROC_BOOT } else { MEM_COPROC }), false).await?;
                let s = self.status().await?;
                if s.status != 0 {
                    return Err(Error::Device(format!("DFU: relay select status 0x{:02x}", s.status)));
                }
            }
            self.command(&frame::coproc_block(chunk), true).await?;
            ev.progress("relay", i as u64 + 1, n as u64);
        }
        let dt = (crate::now_ms() - t1) / 1000.0;
        ev.step(&format!(
            "{n} blocks in {dt:.1} s: {:.1} KB/s; the whole image ({total} blocks) at this rate: {:.0} s",
            n as f64 / dt,
            total as f64 * dt / n as f64
        ));
        if opts.max_blocks.is_some() {
            ev.step(
                "a measurement: no end of transfer. The module's app partition is INVALID now (BL2 writes as it \
                     receives): install a complete image before it runs an app again",
            );
            return Ok(());
        }
        self.usb.sleep(2000).await;
        self.select_ena().await?;
        self.command(&frame::eot(if opts.bl2 { 8 } else { 7 }), false).await?;
        ev.step("relay complete: the module installs the image itself");
        Ok(())
    }
}

/// The relay's options (stock updater's defaults).
pub struct Relay {
    pub bl2: bool,
    pub t1: u16,
    pub t2: u16,
    pub wait_s: u32,
    pub max_blocks: Option<usize>,
    pub handover_only: bool,
}

impl Default for Relay {
    fn default() -> Relay {
        Relay { bl2: false, t1: 5, t2: 0, wait_s: 300, max_blocks: None, handover_only: false }
    }
}

/// Read the app region and the user page (the way back before a flash), and the bootloader for the record.
pub async fn backup<U: Usb>(d: &mut Dfu<U>, ev: &dyn Events) -> Result<Backup> {
    ev.step(&format!("reading the app flash 0x{BOOTLOADER_END:06x}..0x{FLASH_SIZE:06x}"));
    let app = d.read(BOOTLOADER_END, FLASH_SIZE, MEM_FLASH, ev).await?;
    let user = match d.read(0, USER_PAGE_SIZE, MEM_USER, ev).await {
        Ok(u) => Some(u),
        Err(e) => {
            ev.step(&format!("the user page didn't read: {e}"));
            None
        }
    };
    ev.step(&format!("reading the bootloader 0x000000..0x{BOOTLOADER_END:06x}"));
    let bootloader = match d.read(0, BOOTLOADER_END, MEM_FLASH, ev).await {
        Ok(b) => Some(b),
        Err(e) => {
            ev.step(&format!("the bootloader didn't read: {e}"));
            None
        }
    };
    Ok(Backup::new(app, user, bootloader))
}

/// Erase, program flash offsets [start, start + len), read it back; `data` is the image's bytes for that span.
pub async fn write_verified<U: Usb>(d: &mut Dfu<U>, start: u32, data: &[u8], ev: &dyn Events) -> Result<()> {
    ev.step("erasing the app region");
    d.erase().await?;
    ev.step("programming");
    d.program(start, data, ev).await?;
    ev.step("verifying");
    let back = d.read(start, start + data.len() as u32, MEM_FLASH, ev).await?;
    if back != data {
        let i = back.iter().zip(data).position(|(a, b)| a != b).unwrap_or(0);
        return Err(Error::Failed(format!(
            "VERIFY FAILED at app offset 0x{:x} (0x{:02x}, not 0x{:02x})",
            start as usize + i,
            back[i],
            data[i]
        )));
    }
    ev.step(&format!("verify OK ({} bytes, sha {})", back.len(), &crate::hex(&crate::sha256(&back))[..16]));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::frame::*;
    use super::*;

    /// The frames against the decompiled stock updater's constants.
    #[test]
    fn frames() {
        let h = |v: Vec<u8>| crate::hex(&v);
        for (got, want) in [
            (select_ena(1), "06060100"),
            (select_memory(MEM_FLASH), "06030000"),
            (select_memory(MEM_USER), "06030006"),
            (select_memory(MEM_COPROC), "06030007"),
            (select_page(0x1234), "0603011234"),
            (erase_chip(), "0400ff"),
            (start_app(), "0403010000"),
            (start_bl2(), "0404000000"),
            (read_mem(0, 0x3FF), "0300000003ff"),
            (eot(7), "06040700"),
            (eot(8), "06040800"),
            (eot(9), "06040900"),
            (coproc_init(5, 0x1388), "060900051388"),
        ] {
            assert_eq!(h(got), want);
        }
        let pb = program_block(0x0005, &[0xAA, 0xBB]);
        let off = 0x05 + 0x40;
        assert_eq!(pb[..6], [0x01, 0x00, 0x00, 0x05, 0x00, 0x06]);
        assert_eq!(pb[off..off + 2], [0xAA, 0xBB]);
        assert_eq!(pb[off + 2..], suffix());
        assert!(pb[6..off].iter().all(|b| *b == 0));
        let cb = coproc_block(&[0x11, 0x22, 0x33]);
        assert_eq!(cb[..6], [0x01, 0x00, 0x00, 0x00, 0x03, 0xFF]);
        assert_eq!(cb[0x40..0x43], [0x11, 0x22, 0x33]);
        assert!(cb[0x43..0x440].iter().all(|b| *b == 0));
        assert_eq!((cb[0x440..].to_vec(), cb.len()), (suffix().to_vec(), 0x40 + 0x400 + 16));
        assert_eq!(suffix()[4..8], [0x10, b'D', b'F', b'U']);
    }
}
