//! The module's PersParam store (docs/module/boot-chain.md, tools/zeppelin/recover/persparam.py): NAND logical block 1
//! (0x20000), a 0x10-byte header + "PersParam", then records {mark (08080808 live, 0 superseded), record length,
//! data length, 0, key length 4, key byte-reversed, data}. The unit's MAC is MAC0, the only per-unit value; BL2 and
//! the stock app own the store, we only read it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::io::AsRawFd;
use std::sync::OnceLock;
use zd1_sys::mtd;

/// The vendor area, read-only: mtd0 is the `vendor` partition (mtdparts on the kernel command line, os.toml) from
/// NAND 0, or the whole chip on a kernel line without partitions; init makes the node (char 90:1).
pub const DEV: &str = "/dev/mtd0ro";

pub const HDR: [u8; 28] = *b"\x80\x80\x80\x80\x00\x00\x02\x00\x00\x00\x02\x00\x00\x00\x00\x00PersParam\0\0\0";
const LIVE: u32 = 0x0808_0808;

/// Pages exactly as stored: `count` x (2048 data + 64 spare) from byte `offset`. The Nokia kernel runs sulu_nand with
/// Linux soft ECC at the default spare positions (40-63), where this chip has no codes (0xff): a normal read "corrects"
/// a good bit in about half of all 256-byte halves. MTD_MODE_RAW reads around that; the vendor ECC (codes at spare
/// 0x10, tools/zeppelin/nand/ecc.py) is then the only correction to apply.
/// `oob` false: spare areas are not read (returned as 0xff). The boot path reads that way: the PersParam page is
/// appended to in place and is past its fourth write on this unit, so it carries no valid code to correct with, and in
/// the emulator a MEMREADOOB during boot is followed by sulu_eth's "tx DMA queue full" panic under os/snapcast
/// (unexplained; on the unit MEMREADOOB ran on all 65536 pages while streaming the backup over Ethernet, 2026-10-23).
pub fn read_raw(dev: &str, offset: u64, count: usize, oob: bool) -> Result<Vec<u8>, String> {
    let mut f = File::open(dev).map_err(|e| e.to_string())?;
    mtd::raw_mode(f.as_raw_fd()).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(count * 2112);
    for n in 0..count as u64 {
        let at = offset + n * 2048;
        let mut d = vec![0u8; 2048];
        f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        f.read_exact(&mut d).map_err(|e| e.to_string())?;
        out.extend(d);
        if oob {
            out.extend(mtd::read_oob(f.as_raw_fd(), at as u32, 64).map_err(|e| e.to_string())?);
        } else {
            out.extend([0xffu8; 64]);
        }
    }
    Ok(out)
}

/// The spare area of one page (64 bytes on this chip), raw.
#[cfg(feature = "debug")]
pub fn read_oob(dev: &str, page_offset: u64, oob_len: usize) -> Result<Vec<u8>, String> {
    let f = File::open(dev).map_err(|e| e.to_string())?;
    mtd::read_oob(f.as_raw_fd(), page_offset as u32, oob_len).map_err(|e| e.to_string())
}

/// Logical block 1 of the NAND (bad blocks skipped).
pub fn read(dev: &str) -> Result<Vec<u8>, String> {
    let mut f = File::open(dev).map_err(|e| e.to_string())?;
    let fd = f.as_raw_fd();
    let erase = mtd::info(fd).map_or(0x20000, |i| i.erasesize as u64);
    let (mut block, mut good) = (0u64, 0);
    loop {
        let bad = mtd::is_bad(fd, (block * erase) as u32).unwrap_or(false); // an error: not MTD / no bad blocks
        if !bad {
            if good == 1 {
                break;
            }
            good += 1;
        }
        block += 1;
        if block > 16 {
            return Err("no good block for PersParam".into());
        }
    }
    let at = block * erase;
    if let Ok(raw) = read_raw(dev, at, (erase / 2048) as usize, false) {
        return Ok(raw.chunks(2112).flat_map(|p| p[..2048].to_vec()).collect()); // as stored, no kernel ECC
    }
    // not an MTD device (an image file): plain read
    let mut blob = vec![0u8; erase as usize];
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    f.read_exact(&mut blob).map_err(|e| e.to_string())?;
    Ok(blob)
}

/// The unit's MAC from the NAND, read once per process: it never changes, and zd1-linkd asks for it (the default name)
/// every second.
pub fn unit_mac() -> Result<[u8; 6], String> {
    static MAC: OnceLock<Result<[u8; 6], String>> = OnceLock::new();
    MAC.get_or_init(|| mac(&parse(&read(DEV)?)?)).clone()
}

#[derive(Debug, PartialEq)]
pub struct Record<'a> {
    pub key: String,
    pub live: bool,
    pub data: &'a [u8],
}

pub fn parse(blob: &[u8]) -> Result<Vec<Record<'_>>, String> {
    if !blob.starts_with(&HDR) {
        return Err("no PersParam header".into());
    }
    let w = |o: usize| u32::from_le_bytes(blob[o..o + 4].try_into().unwrap());
    let (mut off, mut recs) = (HDR.len(), Vec::new());
    while off + 24 <= blob.len() {
        let (mark, rlen, dlen, klen) = (w(off), w(off + 4) as usize, w(off + 8) as usize, w(off + 16));
        if mark == 0xFFFF_FFFF {
            break;
        }
        let fits =
            dlen.checked_add(24).is_some_and(|n| n <= rlen) && off.checked_add(rlen).is_some_and(|e| e <= blob.len());
        if klen != 4 || !fits || rlen % 4 != 0 {
            // A torn record (power loss mid-write; the unit has one at +0x244, 2026-10-23) ends the store: what came
            // before it is still good (MAC0 is written at the factory, long before the app's SAFE toggles).
            eprintln!("zd1-persparam: bad record at +{off:#x}, using the {} records before it", recs.len());
            break;
        }
        let key = blob[off + 20..off + 24].iter().rev().map(|&b| b as char).collect();
        recs.push(Record { key, live: mark == LIVE, data: &blob[off + 24..off + 24 + dlen] });
        off += rlen;
    }
    Ok(recs)
}

/// The last live record of `key`.
pub fn get<'a>(recs: &[Record<'a>], key: &str) -> Option<&'a [u8]> {
    recs.iter().rev().find(|r| r.live && r.key == key).map(|r| r.data)
}

/// MAC0, if it is a usable unicast address (BL2's rebuilt defaults leave it zero).
pub fn mac(recs: &[Record]) -> Result<[u8; 6], String> {
    let d = get(recs, "MAC0").ok_or("no MAC0")?;
    let m: [u8; 6] = d.get(..6).and_then(|d| d.try_into().ok()).ok_or(format!("MAC0 is {} bytes", d.len()))?;
    if m == [0; 6] || m == [0xFF; 6] {
        return Err("MAC0 not set (BL2 defaults?)".into());
    }
    if m[0] & 1 != 0 {
        return Err("MAC0 is a multicast address".into());
    }
    Ok(m)
}

/// The Wi-Fi interface's own MAC: MAC0 with the locally administered bit set (ec:66:d1:.. -> ee:66:d1:..). The stock
/// firmware used MAC0 on both, one interface at a time; with both up on one LAN that confuses switches, DHCP, ARP and
/// mDNS. MAC0 + 1 would be the next unit's MAC if B&W numbered them in sequence; a local address can't clash with a
/// factory one. (A MAC0 that is already local gets its last bit flipped instead, so the two still differ.)
pub fn wlan_mac(m: &[u8; 6]) -> [u8; 6] {
    let mut w = *m;
    w[0] |= 0x02;
    if w == *m {
        w[5] ^= 0x01;
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use zd1_sys::format_mac;

    fn record(key: &str, data: &[u8], live: bool) -> Vec<u8> {
        let rlen = (24 + data.len() + 3) & !3;
        let mut r = Vec::new();
        for v in [if live { LIVE } else { 0 }, rlen as u32, data.len() as u32, 0, 4] {
            r.extend(v.to_le_bytes());
        }
        r.extend(key.bytes().rev());
        r.extend(data);
        r.resize(rlen, 0xFF);
        r
    }

    /// Like persparam.py --factory: the zero MAC0 superseded by the unit's.
    fn factory(mac: &[u8]) -> Vec<u8> {
        let mut b = HDR.to_vec();
        for (k, d, live) in [
            ("HWID", &[0xBD, 0, 0, 0][..], true),
            ("MAC0", &[0; 8][..], false),
            ("BLDF", b"081008", true),
            ("MAC0", mac, true),
            ("DTCV", &[1, 0, 0, 0], true),
        ] {
            b.extend(record(k, d, live));
        }
        b.resize(0x20000, 0xFF);
        b
    }

    #[test]
    fn the_unit_s_mac() {
        let b = factory(&[0xEC, 0x66, 0xD1, 0, 0, 0x42]);
        let r = parse(&b).unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(get(&r, "HWID"), Some(&[0xBD, 0, 0, 0][..]));
        assert_eq!(format_mac(&mac(&r).unwrap()), "ec:66:d1:00:00:42");
        assert_eq!(format_mac(&wlan_mac(&mac(&r).unwrap())), "ee:66:d1:00:00:42");
        assert_eq!(format_mac(&wlan_mac(&[0x02, 0x5A, 0xD1, 0, 0, 1])), "02:5a:d1:00:00:00");
    }

    #[test]
    fn bl2_defaults_and_damage() {
        let mut b = HDR.to_vec();
        b.extend(record("MAC0", &[0; 8], true));
        b.resize(0x800, 0xFF);
        assert!(mac(&parse(&b).unwrap()).unwrap_err().contains("not set"));
        assert!(parse(&[0xFF; 64]).is_err());
        let mut b = factory(&[0xEC, 0x66, 0xD1, 0, 0, 0x42]);
        b[HDR.len() + 16] = 5; // key length: a damaged first record leaves no records, so no MAC
        assert!(mac(&parse(&b).unwrap()).is_err());
        let mut b = factory(&[0xEC, 0x66, 0xD1, 0, 0, 0x42]);
        b[HDR.len() + 8..HDR.len() + 12].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // a data length that wraps
        b[HDR.len() + 4..HDR.len() + 8].copy_from_slice(&0xFFFF_FFFCu32.to_le_bytes());
        assert!(parse(&b).unwrap().is_empty());
        assert!(mac(&parse(&factory(&[1, 2, 3, 4, 5, 6])).unwrap()).is_err());
    }
}

// the program (zd1-persparam), in the multicall (module/os/apps/zd1)
mod cli;
pub use cli::main;
