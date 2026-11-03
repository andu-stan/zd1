//! zd1-board: the module's board by name (module/dm870).
//!   zd1-board [show]    the chip's state: clocks, gates, resets, PLLs, the pad routing, every GPIO, the timers
//!   zd1-board up        the bring-up init does at boot (the pads, the PHY's reset and clock, the gates): again

use dm870::DevMem;

pub fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "show".into());
    let mut mem = match DevMem::open() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("zd1-board: /dev/mem: {e}");
            std::process::exit(1);
        }
    };
    match arg.as_str() {
        "show" => print!("{}", crate::report(&mut mem)),
        "up" => {
            crate::up(&mut mem);
            println!("zd1-board: up");
        }
        _ => {
            eprintln!("usage: zd1-board [show | up]");
            std::process::exit(2);
        }
    }
}
