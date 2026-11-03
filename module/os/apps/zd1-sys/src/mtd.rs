//! The MTD ioctls (linux/mtd/mtd-abi.h): the chip's geometry, bad blocks, erasing, the spare areas, raw reads.

use std::io;
use std::os::fd::RawFd;

const MEMGETINFO: libc::c_ulong = 0x8020_4D01; // _IOR('M', 1, struct mtd_info_user)
const MEMERASE: libc::c_ulong = 0x4008_4D02; // _IOW('M', 2, struct erase_info_user)
const MEMWRITEOOB: libc::c_ulong = 0xC00C_4D03; // _IOWR('M', 3, struct mtd_oob_buf)
const MEMREADOOB: libc::c_ulong = 0xC00C_4D04; // _IOWR('M', 4, struct mtd_oob_buf)
const MEMGETOOBSEL: libc::c_ulong = 0x80C8_4D0A; // _IOR('M', 10, struct nand_oobinfo)
const MEMGETBADBLOCK: libc::c_ulong = 0x4008_4D0B; // _IOW('M', 11, loff_t)
const MTDFILEMODE: libc::c_ulong = 0x4D13; // _IO('M', 19)
const MTD_MODE_RAW: libc::c_ulong = 3;
pub const NANDFLASH: u8 = 4;
const NANDECC_AUTOPLACE: u32 = 2;

/// `struct mtd_info_user`.
#[repr(C)]
#[derive(Default, Debug)]
pub struct Info {
    pub kind: u8,
    pub flags: u32,
    pub size: u32,
    pub erasesize: u32,
    pub writesize: u32,
    pub oobsize: u32,
    pub ecctype: u32,
    pub eccsize: u32,
}

#[repr(C)]
struct EraseInfo {
    start: u32,
    length: u32,
}

#[repr(C)]
struct OobBuf {
    start: u32,
    length: u32,
    ptr: *mut u8,
}

#[repr(C)]
struct NandOobinfo {
    useecc: u32,
    eccbytes: u32,
    oobfree: [[u32; 2]; 8],
    eccpos: [u32; 32],
}

fn ioctl<T>(fd: RawFd, req: libc::c_ulong, arg: *mut T, what: &str) -> io::Result<libc::c_int> {
    let r = unsafe { libc::ioctl(fd, req as _, arg) };
    if r < 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(e.kind(), format!("{what}: {e}")));
    }
    Ok(r)
}

pub fn info(fd: RawFd) -> io::Result<Info> {
    let mut i = Info::default();
    ioctl(fd, MEMGETINFO, &mut i, "MEMGETINFO")?;
    Ok(i)
}

/// Whether the block at byte `offset` is marked bad.
pub fn is_bad(fd: RawFd, offset: u32) -> io::Result<bool> {
    let mut off = offset as i64;
    Ok(ioctl(fd, MEMGETBADBLOCK, &mut off, "MEMGETBADBLOCK")? > 0)
}

pub fn erase(fd: RawFd, start: u32, length: u32) -> io::Result<()> {
    ioctl(fd, MEMERASE, &mut EraseInfo { start, length }, &format!("erasing at {start:#x}")).map(|_| ())
}

/// Bytes into the spare area of the page at byte `offset`.
pub fn write_oob(fd: RawFd, offset: u32, data: &[u8]) -> io::Result<()> {
    let mut b = OobBuf { start: offset, length: data.len() as u32, ptr: data.as_ptr() as *mut u8 };
    ioctl(fd, MEMWRITEOOB, &mut b, &format!("spare area at {offset:#x}")).map(|_| ())
}

/// `len` bytes of the spare area of the page at byte `offset`, as stored.
pub fn read_oob(fd: RawFd, offset: u32, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut b = OobBuf { start: offset, length: len as u32, ptr: buf.as_mut_ptr() };
    ioctl(fd, MEMREADOOB, &mut b, "MEMREADOOB")?;
    buf.truncate(b.length as usize);
    Ok(buf)
}

/// The first free bytes of the spare area (where JFFS2 puts its cleanmarker): offset and length, at most 8.
pub fn oob_free(fd: RawFd, info: &Info) -> io::Result<(u32, u32)> {
    let mut oi: NandOobinfo = unsafe { std::mem::zeroed() };
    ioctl(fd, MEMGETOOBSEL, &mut oi, "MEMGETOOBSEL")?;
    Ok(if oi.useecc == NANDECC_AUTOPLACE {
        (oi.oobfree[0][0], oi.oobfree[0][1].min(8))
    } else {
        match info.oobsize {
            8 => (6, 2),
            16 => (8, 8),
            _ => (16, 8),
        }
    })
}

/// Reads on `fd` from now on as stored: no ECC correction by the kernel.
pub fn raw_mode(fd: RawFd) -> io::Result<()> {
    if unsafe { libc::ioctl(fd, MTDFILEMODE as _, MTD_MODE_RAW) } != 0 {
        return Err(io::Error::other("MTDFILEMODE raw refused"));
    }
    Ok(())
}
