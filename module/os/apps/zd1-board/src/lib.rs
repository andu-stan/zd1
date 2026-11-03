//! The module's board under Linux: what the Nokia kernel leaves unset on this carrier, set once at boot ([`up`]), and
//! the chip's state by name ([`report`]). Found over JTAG on the unit by diffing the register state under BL2 against
//! Linux and replaying the differences [HW 2026-10-23]; every value and rule is the PAC's (module/dm870).

use dm870::Mmio;
use dm870::clocks::{Gate, Gates, Pll, PllSetting, Reset, misc, tgen};
use dm870::gpio::{Level, Mode, Pin, gpio};
use dm870::pads::{self, Mtx, pads};
use dm870::timers::{self, timers};
use std::fmt::Write;

/// Bring the board up: the pad routing, the Ethernet PHY out of reset and its clock (AV1), the clock gates the stock
/// app has on and the SPI slave's. Run once, after the kernel's modules are loaded: the values the drivers own (the
/// Wi-Fi core's reset and hold bit) are left as they are.
pub fn up(m: &mut impl Mmio) {
    pads(m).route(&pads::BL2);
    gpio(m).drive(Pin::NresetEth, Level::High);
    let mut t = tgen(m);
    t.enable(Gates::STOCK_APP.and(Gates::SPI_SLAVE));
    t.set_misc_stock();
    t.write_stock_unknown();
    t.program(Pll::AV1, PllSetting::AV1_PHY);
}

/// The chip's state as text, a section per block: what `zd1-board show` prints.
pub fn report(m: &mut impl Mmio) -> String {
    let mut o = String::new();
    let t = tgen(m).tree();
    let domains: Vec<String> = t.domains.iter().map(|(n, hz)| format!("{n} {}", mhz(*hz))).collect();
    let _ = writeln!(o, "clocks   root {}, AHB {}: {}", mhz(t.root_hz), mhz(t.ahb_hz), domains.join(", "));

    let on = tgen(m).gates();
    let _ = writeln!(o, "gates    0x{:08X} (the named ones, and the others that are off or the stock app's)", on.0);
    for n in 0..=Gate::LAST.0 {
        let g = Gate(n);
        let state = if Gates::READ_AS_ZERO.contains(g) {
            "?  (reads 0)"
        } else if on.contains(g) {
            "on"
        } else {
            "off"
        };
        let stock = if Gates::STOCK_APP.contains(g) { "  stock app's" } else { "" };
        if let Some(name) = g.name() {
            let _ = writeln!(o, "         {n:>2} {name:<18} {state}{stock}");
        } else if !on.contains(g) || Gates::STOCK_APP.contains(g) {
            let _ = writeln!(o, "         {n:>2} {:<18} {state}{stock}", "");
        }
    }

    let held = tgen(m).in_reset(Reset::WILD_ARM7);
    let _ = writeln!(o, "reset    Wi-Fi ARM7 {}", if held { "held in reset" } else { "running" });
    let v = tgen(m).misc();
    let _ =
        writeln!(o, "misc     0x{v:08X}: Wi-Fi hold {}, stock bit {}", bit(v & misc::WILD_HOLD), bit(v & misc::STOCK));
    for (name, pll, expect) in [("AV1", Pll::AV1, Some(PllSetting::AV1_PHY)), ("av_clock", Pll::AV_CLOCK, None)] {
        let s = tgen(m).pll(pll);
        let as_set = match expect {
            Some(e) if e == s => " (BL2's: the Ethernet PHY's clock)",
            Some(_) => " (not BL2's)",
            None => "",
        };
        let _ = writeln!(
            o,
            "pll      {name:<8} {} 0x{:08X} 0x{:08X} 0x{:08X}{as_set}",
            if s.running() { "running" } else { "off    " },
            s.control,
            s.config[0],
            s.config[1]
        );
    }

    if pads(m).holds(&pads::BL2) {
        let _ = writeln!(o, "pads     BL2's routing in all {} copies", pads::COPIES);
    } else {
        let _ = writeln!(o, "pads     NOT BL2's routing:");
        for copy in 0..pads::COPIES {
            for &(reg, want) in pads::BL2.0 {
                let got = pads(m).get(copy, reg);
                if got != want {
                    let _ = writeln!(o, "         copy {copy:>2} {:<16} 0x{got:X} (BL2: 0x{want:X})", reg.name());
                }
            }
        }
    }
    let others: Vec<String> = [Mtx::Others, Mtx::AvLoop, Mtx::TestBusSel, Mtx::TestBusEnable]
        .iter()
        .map(|&r| format!("{} 0x{:X}", r.name(), pads(m).get(0, r)))
        .collect();
    let _ = writeln!(o, "         (not routed by us: {})", others.join(", "));

    for pin in Pin::ALL {
        let mode = match gpio(m).mode(pin) {
            Mode::Drives(Level::High) => "drives high".to_string(),
            Mode::Drives(Level::Low) => "drives low".to_string(),
            Mode::Released { pull_up } => format!("released{}", if pull_up { ", pull-up" } else { "" }),
        };
        let level = if gpio(m).level(pin) == Level::High { "high" } else { "low" };
        let head = if pin as u8 == 0 { "gpio" } else { "" };
        let _ = writeln!(o, "{head:<8} {:>2} {:<14} {mode:<20} {level}", pin as u8, pin.name());
    }

    let c = timers(m).config();
    let running: Vec<String> = (0..3).filter(|&n| c & timers::running(n) != 0).map(|n| n.to_string()).collect();
    let _ = writeln!(
        o,
        "timers   0x{c:08X}: running {}{}{}",
        if running.is_empty() { "none".into() } else { running.join(", ") },
        if c & timers::TIMESTAMP_ON != 0 { "; timestamp on" } else { "" },
        if c & timers::RESET_ON_EXPIRY != 0 { "; WATCHDOG ARMED" } else { "" }
    );
    o
}

fn mhz(hz: u32) -> String {
    if hz % 1_000_000 == 0 { format!("{} MHz", hz / 1_000_000) } else { format!("{:.3} MHz", hz as f64 / 1e6) }
}

fn bit(v: u32) -> &'static str {
    if v != 0 { "set" } else { "clear" }
}

// the program (zd1-board), in the multicall (module/os/apps/zd1)
mod cli;
pub use cli::main;
