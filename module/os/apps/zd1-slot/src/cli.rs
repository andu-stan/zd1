//! zd1-slot: the system slots (docs/module/storage.md; the library has how they work). What they hold: `zd1-ctl status`.
//!   zd1-slot mtd NAME           the MTD number of partition NAME (system-a, system-b, data, vendor), its nodes made
//!   zd1-slot install SLOT ID    a gzip'd cpio of a root on stdin into SLOT (a, b), stamped ID
//! Which slot starts is the MCU's (`zd1 slot`).

use crate::{Slot, install, mtd};

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["mtd", name] => match mtd(name) {
            Some(n) => println!("{n}"),
            None => die(&format!("no partition \"{name}\"")),
        },
        ["install", s, id] => {
            let Some(s) = Slot::parse(s) else { die("SLOT: a or b") };
            match install(s, id, &mut std::io::stdin().lock(), &mut |_| Ok(())) {
                Ok(m) => println!("{m}"),
                Err(e) => die(&e),
            }
        }
        _ => {
            eprintln!("usage: zd1-slot mtd NAME | install SLOT ID < IMAGE");
            std::process::exit(2);
        }
    }
}

fn die(e: &str) -> ! {
    zd1_sys::die("zd1-slot", e)
}
