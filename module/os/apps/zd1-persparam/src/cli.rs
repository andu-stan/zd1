//! zd1-persparam: the unit's PersParam store, read-only, from the whole-NAND MTD device (Linux sulu_nand registers
//! one `mtd0` without partitions; /dev/mtd0ro = char 90:1).
//!   zd1-persparam mac            the unit's MAC (MAC0), aa:bb:cc:dd:ee:ff; exit 1 if it isn't set
//!   zd1-persparam mac --wlan     the Wi-Fi MAC (MAC0, locally administered: its own address), as sulu_wlan's
//!                                `macaddr=` parameter (0xee,0xbb,...)
//!   zd1-persparam get KEY        a record's data in hex
//!   zd1-persparam list           the live records (MAC0 / MAC1 / GUID masked)
//!   zd1-persparam oob OFFSET [LEN]  (the debug build) the spare area of the page at byte OFFSET (hex), raw: the ECC
//!                                slots
//!   zd1-persparam raw OFFSET COUNT  (the debug build) COUNT pages from byte OFFSET (hex) as stored, no kernel ECC:
//!                                2048 data + 64 spare each, binary to stdout
//!   --dev PATH                   default /dev/mtd0ro; a NAND image file works too
//! PersParam is logical block 1: like the stock stage 1, bad blocks don't count.

pub fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut dev = crate::DEV.to_string();
    if let Some(i) = args.iter().position(|a| a == "--dev") {
        if i + 1 < args.len() {
            dev = args.remove(i + 1);
        }
        args.remove(i);
    }
    #[cfg(feature = "debug")]
    if raw(&args, &dev) {
        return;
    }
    let blob = match crate::read(&dev) {
        Ok(b) => b,
        Err(e) => die(&format!("{dev}: {e}")),
    };
    let recs = match crate::parse(&blob) {
        Ok(r) => r,
        Err(e) => die(&e),
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["mac"] | ["mac", "--wlan"] => match crate::mac(&recs) {
            Ok(m) if args.len() == 2 => {
                println!("{}", crate::wlan_mac(&m).iter().map(|b| format!("{b:#04x}")).collect::<Vec<_>>().join(","))
            }
            Ok(m) => println!("{}", zd1_sys::format_mac(&m)),
            Err(e) => die(&e),
        },
        ["get", key] => match crate::get(&recs, key) {
            Some(d) => println!("{}", d.iter().map(|b| format!("{b:02x}")).collect::<String>()),
            None => die(&format!("no live {key}")),
        },
        ["list"] => {
            for r in recs.iter().filter(|r| r.live) {
                let v = if ["MAC0", "MAC1", "GUID"].contains(&r.key.as_str()) {
                    "(masked)".to_string()
                } else {
                    r.data.iter().map(|b| format!("{b:02x}")).collect()
                };
                println!("{} {v}", r.key);
            }
        }
        _ => {
            eprintln!("usage: zd1-persparam [--dev PATH] mac [--wlan] | get KEY | list");
            std::process::exit(2);
        }
    }
}

/// The debug build's `oob` and `raw`: the NAND as stored. Whether one was asked for (and done).
#[cfg(feature = "debug")]
fn raw(args: &[String], dev: &str) -> bool {
    let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or_else(|_| die("bad offset"));
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["oob", off, ref len @ ..] => {
            let len = len.first().and_then(|l| l.parse().ok()).unwrap_or(64usize);
            match crate::read_oob(dev, hex(off), len) {
                Ok(b) => println!("{}", zd1_sys::hex(&b)),
                Err(e) => die(&e),
            }
        }
        ["raw", off, count] => {
            use std::io::Write;
            let (off, n): (u64, u64) = (hex(off), count.parse().unwrap_or_else(|_| die("bad count")));
            let mut out = std::io::stdout().lock();
            for p in 0..n {
                // a page at a time: the whole chip doesn't fit in RAM
                match crate::read_raw(dev, off + p * 2048, 1, true) {
                    Ok(b) => out.write_all(&b).unwrap_or_else(|e| die(&e.to_string())),
                    Err(e) => die(&e),
                }
            }
        }
        _ => return false,
    }
    true
}

fn die(e: &str) -> ! {
    zd1_sys::die("zd1-persparam", e)
}
