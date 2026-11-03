//! zd1: the Zeppelin Air over USB from a terminal. Every flow is host/core's (the web page runs the same ones); this
//! is the command line around them. Real USB, or the emulator's bridge with `ZD1DFU_EMU=host:port`.

mod dfu;
mod files;
mod fmt;
mod term;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Parser, Subcommand, ValueEnum};
use files::{read, repo};
use futures_lite::future::block_on;
use std::path::PathBuf;
use std::rc::Rc;
use term::Term;
use zd1_host::image::McuImage;
use zd1_host::spec::{BootOp, Key, ModOp, Slot, SpecEnum};
use zd1_host::usb::{Bus, EmuBus, NusbBus, Usb as _};
use zd1_host::{Error, Events, Mcu, Result, console, flash, install, lab, msg, settings, speaker, update, xmodem};

#[derive(Parser)]
#[command(
    name = "zd1",
    version,
    about = "The Zeppelin Air over USB: our MCU firmware's host link (docs/protocol/host.md), \
    the module OS's settings, updates of both boards, the DFU bootloader. With ZD1DFU_EMU=host:port, the emulator's.",
    after_help = "Exit status: 0 done; 1 failed (the MCU refused, a flow's verdict); 69 the speaker isn't on USB or \
    left it; 77 the gate is locked or nobody pressed ON/OFF; 124 no answer in time; 130 stopped (Ctrl-C)."
)]
struct Cli {
    /// Wait this long (s) for the speaker to come on USB.
    #[arg(long, global = true, default_value_t = 0.0)]
    wait: f64,
    /// Print the MCU's log as the command reads it (the module's console among it). Every command's polls drain the
    /// log: without this, those that don't report in it (`update`, `info`, ...) drop what they read.
    #[arg(long, global = true)]
    log: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// The speaker at a glance: the MCU, the source and amp, the network module, the gate.
    Info,
    /// The firmware's internals: protections (temperature), audio path, USB audio, the module's link, the remote.
    Diag,
    /// The firmware's version (answered from the interrupt: works while the MCU is busy).
    Hello,
    /// What image files are, as JSON lines (kind, version, size, SHA-256): MCU firmware, a backup, a system image, a .bcd. The
    /// release manifest the web page reads is made of these (tools/zeppelin/web.py, docs/web.md).
    Image { files: Vec<PathBuf> },
    /// The MCU's log since it was last read (the module's console among it).
    Log {
        /// Keep following (Ctrl-C ends).
        #[arg(short, long)]
        follow: bool,
    },
    /// Play a source; a receiver of the module OS needs a sender connected.
    Play {
        #[arg(value_parser = PossibleValuesParser::new(speaker::PLAY))]
        what: String,
    },
    /// Standby.
    Standby,
    /// The volume, 0 (mute) .. 71; and the bass, 0 .. 4.
    Volume {
        step: Option<u8>,
        #[arg(long)]
        bass: Option<u8>,
    },
    /// A key, as if pressed on the speaker or the remote.
    Key {
        #[arg(value_parser = spec_enum::<Key>())]
        key: Key,
    },
    /// The amp: allowed until the MCU restarts, allowed from every boot (the default), or locked (also at boot).
    Amp { mode: AmpMode },
    /// The speaker's behaviour, kept in the EEPROM.
    Prefs {
        /// A USB stream that starts takes the speaker by itself.
        #[arg(long)]
        auto_switch: Option<OnOff>,
        /// A source silent this many minutes switches the amp off (1 .. 30; 5 unless set).
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=30))]
        amp_idle: Option<u16>,
        /// The highest volume step on every source, whatever sets it (keys, remote, the computer, the network's
        /// senders), 1 .. 71 (40 unless set); a lower cap takes the volume down to it.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=71))]
        volume_cap: Option<u8>,
    },
    /// The gate in front of requests that change the speaker: its state, or ask for a press of ON/OFF now, lock it,
    /// or open it for good (a unit tools drive unattended).
    Gate { action: Option<GateAction> },
    /// The module OS's settings (docs/module/settings.md): all of them, one, or one changed ("" = its default); an
    /// action's key does it (wifi.add, wifi.forget, wifi.first, airplay.forget, reset).
    Settings {
        key: Option<String>,
        value: Option<String>,
        /// The settings that differ from their defaults, as KEY=VALUE lines (what --import takes; no secrets).
        #[arg(long, conflicts_with_all = ["key", "import"])]
        export: bool,
        /// Write the settings in this file (KEY=VALUE lines, `--export`'s).
        #[arg(long, conflicts_with = "key")]
        import: Option<PathBuf>,
    },
    /// The module back to its defaults: every setting; with --all the known Wi-Fi networks and AirPlay's pairings too.
    Reset {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
    },
    /// The controllers paired with AirPlay (with a PIN, `zd1 settings airplay.pin 1234`), or all of them forgotten.
    Pairings {
        #[arg(long)]
        forget: bool,
    },
    /// The snapservers on the speaker's networks (mDNS), for `zd1 settings snapcast.server HOST:PORT`.
    Servers,
    /// The module OS's State record: every field (its settings and its status; docs/module/settings.md).
    State,
    /// Our firmware on both boards, from any state: the module's boot image through the bootloader's relay, the MCU
    /// firmware (a backup first), the module's first boot, the system. A stock speaker: hold ON/OFF while plugging it
    /// in (DFU). Defaults: this checkout's builds.
    Install {
        /// The module's boot image (os.bcd: the stub, the kernel, the rescue).
        #[arg(long)]
        bcd: Option<PathBuf>,
        /// It is on the module already (a repair: the MCU firmware and the system only).
        #[arg(long, conflicts_with = "bcd")]
        no_bcd: bool,
        /// The MCU firmware (Intel HEX or the app region's .bin).
        #[arg(long)]
        mcu: Option<PathBuf>,
        /// The system image (system.cpio.gz).
        #[arg(long)]
        system: Option<PathBuf>,
        /// Where the MCU backup goes (units/<unit>; the one unit in units/ by default).
        #[arg(long)]
        unit: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
    },
    /// The stock firmware back on both boards, the module first: the stock .bcd through the relay (~6.5 min), then
    /// the stock MCU firmware (blobs/mcu/zd1-2.04.05.hex, or a backup's app_flash.bin), with a backup of what is on
    /// the MCU first. The speaker comes up unconfigured, as from the factory.
    Rollback {
        /// The stock module image (.bcd).
        bcd: PathBuf,
        /// The stock MCU firmware (.hex, or a backup's app_flash.bin).
        mcu: PathBuf,
        #[arg(long)]
        unit: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
    },
    /// The speaker's name everywhere (AirPlay, Spotify Connect, Snapcast).
    Name { name: Option<String> },
    /// The Wi-Fi: how it is (the network, whether it is joined, the access point and signal, the address), join a
    /// network (the passphrase is asked for here and turned into the WPA2 key on this computer; only the key goes to
    /// the speaker), the networks around the speaker, or none.
    Wifi {
        ssid: Option<String>,
        /// A network without a key.
        #[arg(long, conflicts_with = "passphrase_file")]
        open: bool,
        /// The passphrase from this file's first line.
        #[arg(long)]
        passphrase_file: Option<PathBuf>,
        /// No Wi-Fi.
        #[arg(long, conflicts_with_all = ["ssid", "open", "scan"])]
        off: bool,
        /// The networks the speaker's radio sees (a scan takes a few seconds).
        #[arg(long, conflicts_with_all = ["ssid", "open"])]
        scan: bool,
        /// The networks the speaker knows, the preferred first.
        #[arg(long, conflicts_with_all = ["ssid", "open", "scan", "off"])]
        known: bool,
        /// Forget SSID.
        #[arg(long, requires = "ssid", conflicts_with_all = ["open", "passphrase_file", "first"])]
        forget: bool,
        /// Prefer SSID, a known network (put it first).
        #[arg(long, requires = "ssid", conflicts_with_all = ["open", "passphrase_file"])]
        first: bool,
    },
    /// The Wi-Fi address: dhcp, or ADDRESS/PREFIX.
    Ip {
        /// off, dhcp, or a fixed ADDRESS/PREFIX
        address: String,
        #[arg(long)]
        gateway: Option<String>,
        #[arg(long)]
        dns: Option<String>,
        /// The wired interface (default: Wi-Fi).
        #[arg(long)]
        eth: bool,
    },
    /// A receiver of the module OS on or off.
    Receiver { name: Receiver, state: OnOff },
    /// The module's system into the slot that doesn't run, then that slot started once: kept if it gets ready, else
    /// the MCU goes back (docs/module/updater.md). A .bcd: the module's boot image instead (stub, kernel, rescue),
    /// through the bootloader's relay (~5 min; a module in BL2 with no image: BL2's XModem), the module then up on
    /// its good slot.
    Update {
        /// A slot's root, gzip'd cpio, or our .bcd [default: emu/img/os/system.cpio.gz].
        image: Option<PathBuf>,
        /// Install it, don't start it (`zd1 slot try SLOT --restart` later).
        #[arg(long)]
        no_start: bool,
        /// The module console's speed while it runs.
        #[arg(long, default_value_t = 1_000_000)]
        baud: u32,
        /// A test: ABORT this many seconds into the stream (the MCU must answer, the slot holds nothing).
        #[arg(long, value_name = "S")]
        abort_after: Option<f64>,
        /// The emulator without the MCU's part: zd1-update fed over the module console's socket (ZD1_MODCON_EMU).
        #[arg(long)]
        uart: bool,
    },
    /// Our MCU firmware onto the speaker: into DFU, a backup into UNIT/mcu/dfu-backups/, flash, verify, start, check.
    Flash {
        /// Intel HEX, or the app region's .bin [default: mcu/result/zd1.hex].
        image: Option<PathBuf>,
        /// The unit's directory [default: the one in units/].
        #[arg(long)]
        unit: Option<PathBuf>,
    },
    /// Which system slot the module starts (docs/module/storage.md).
    Slot {
        #[command(subcommand)]
        op: Option<SlotOp>,
        /// Restart the module now.
        #[arg(long, global = true)]
        restart: bool,
    },
    /// The network module's console over the host link (BL1's and BL2's menus, our OS's shell): a terminal on it
    /// (Ctrl-] leaves), shell commands, or what it says.
    Console {
        #[command(subcommand)]
        action: Option<ConsoleAction>,
    },
    /// The network module's power, and into BL2: over SPI (bl2: an image that answers it), or through BL1's serial
    /// menu (esc: whatever the image).
    Module {
        #[arg(value_parser = spec_enum::<ModOp>())]
        op: ModOp,
        /// How long to wait (s; 0: 120) for POWER and BL2.
        #[arg(short, long, default_value_t = 0)]
        timeout: u16,
    },
    /// A module register over SPI.
    Reg {
        #[command(subcommand)]
        op: RegOp,
    },
    /// The module OS's debug services (telnet shell, memory server), until the MCU powers off.
    Debug {
        #[arg(value_parser = spec_enum::<zd1_host::spec::Debug>())]
        mode: zd1_host::spec::Debug,
    },
    /// Read the MCU's memory.
    Peek {
        #[arg(value_parser = number::<u32>)]
        addr: u32,
        #[arg(value_parser = number::<u16>, default_value = "16")]
        len: u16,
        /// 32-bit reads (registers).
        #[arg(short, long)]
        words: bool,
    },
    /// Write the MCU's memory.
    Poke {
        #[arg(value_parser = number::<u32>)]
        addr: u32,
        #[arg(value_parser = hex)]
        data: Hex,
        /// 32-bit writes (registers).
        #[arg(short, long)]
        words: bool,
    },
    /// Restart the MCU's firmware (the module powered off first).
    Reboot,
    /// Into the B&W DFU bootloader (03EB:2FF8).
    Bootloader,
    /// The B&W DFU bootloader itself (`zd1 bootloader` gets there).
    Dfu {
        #[command(subcommand)]
        cmd: dfu::DfuCmd,
    },
    /// The bulk pipe into DISCARD: throughput, and the MCU's CRC-32 against ours.
    BulkTest {
        #[arg(default_value_t = 4.0)]
        mb: f64,
    },
    /// A module image through BL2's XModem over SPI, driven by our MCU. BL2 erases the app partition first.
    Xmodem {
        image: PathBuf,
        /// The module is in BL2 already.
        #[arg(long)]
        in_bl2: bool,
        /// A rehearsal: only N blocks, no EOT (leaves the app partition invalid).
        #[arg(long)]
        blocks: Option<usize>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum AmpMode {
    Allow,
    Auto,
    Deny,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum GateAction {
    Unlock,
    Lock,
    Open,
}

#[derive(Subcommand)]
enum ConsoleAction {
    /// Commands on our OS's shell, one after the other, each to its end: their output, and the exit status of the
    /// last.
    Run {
        /// Seconds per command.
        #[arg(short, long, default_value_t = 30)]
        timeout: u32,
        #[arg(required = true)]
        cmds: Vec<String>,
    },
    /// Print what the console says (Ctrl-C ends).
    Log,
}

#[derive(Clone, Copy, ValueEnum)]
enum Receiver {
    Airplay,
    Spotify,
    Snapcast,
}

#[derive(Clone, Copy, ValueEnum)]
enum TrySlot {
    A,
    B,
    Rescue,
}

#[derive(Clone, Copy, ValueEnum)]
enum GoodSlot {
    A,
    B,
}

#[derive(Subcommand)]
enum SlotOp {
    /// Start a slot once at the module's next boot: it becomes the good one when it gets ready.
    Try { slot: TrySlot },
    /// Make a slot the good one.
    Good { slot: GoodSlot },
    /// Forget a try and the failures since the last ready.
    Clear,
}

#[derive(Subcommand)]
enum RegOp {
    /// F: its two bytes; with --v the variable-length payload.
    Read {
        #[arg(value_parser = number::<u8>)]
        reg: u8,
        #[arg(long)]
        v: bool,
    },
    /// F: two bytes; with --v a variable-length payload.
    Write {
        #[arg(value_parser = number::<u8>)]
        reg: u8,
        #[arg(value_parser = hex)]
        data: Hex,
        #[arg(long)]
        v: bool,
    },
}

/// A spec enum's values as choices, by their lower-case names.
fn spec_enum<E: SpecEnum + Send + Sync>() -> impl TypedValueParser<Value = E> {
    PossibleValuesParser::new(E::ALL.iter().map(|e| e.to_string())).map(|s| s.parse::<E>().expect("one of the choices"))
}

/// A number, decimal or 0x hex, that fits `T`.
pub fn number<T: TryFrom<u64>>(s: &str) -> std::result::Result<T, String> {
    let n = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse(),
    }
    .map_err(|e| e.to_string())?;
    T::try_from(n).map_err(|_| format!("{s}: at most {} bits", 8 * size_of::<T>()))
}

#[derive(Clone)]
struct Hex(Vec<u8>);

/// Hex digits (spaces between them allowed) as bytes.
fn hex(s: &str) -> std::result::Result<Hex, String> {
    let d: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    d.chunks(2)
        .map(|p| match p {
            [a, b] => Some(((*a as char).to_digit(16)? << 4 | (*b as char).to_digit(16)?) as u8),
            _ => None,
        })
        .collect::<Option<Vec<u8>>>()
        .map(Hex)
        .ok_or_else(|| "hex digits, two per byte".into())
}

fn main() {
    let cli = Cli::parse();
    let r = match EmuBus::from_env() {
        Some(b) => block_on(run(&b, cli)),
        None => block_on(run(&NusbBus, cli)),
    };
    match r {
        Ok(c) => std::process::exit(c),
        Err(e) => {
            eprintln!("zd1: {e}");
            std::process::exit(exit_status(&e))
        }
    }
}

/// The exit status for an error (`zd1 --help`).
fn exit_status(e: &Error) -> i32 {
    match e.code() {
        "CANCELLED" => 130,
        "TIMEOUT" => 124,
        "GONE" | "DEVICE" => 69,
        "LOCKED" | "DENIED" => 77,
        _ => 1,
    }
}

async fn run<B: Bus>(bus: &B, cli: Cli) -> Result<i32> {
    let term = Rc::new(Term::default());
    term.show_log(cli.log);
    let ev: Rc<dyn Events> = term.clone();
    let open = |backlog| open_mcu(bus, cli.wait, ev.clone(), backlog);
    match cli.cmd {
        Cmd::Dfu { cmd } => return dfu::dfu_cmd(bus, cmd, ev.clone()).await,
        Cmd::Image { files } => {
            for f in files {
                println!("{}", files::image_json(&f)?);
            }
            return Ok(0);
        }
        Cmd::Flash { image, unit } => return dfu::mcu_flash(bus, image, unit, ev).await,
        Cmd::Install { bcd, no_bcd, mcu, system, unit, yes } => {
            let bcd = if no_bcd { None } else { Some(read(&bcd.unwrap_or_else(|| repo().join("emu/img/os/os.bcd")))?) };
            let mcu_path = mcu.unwrap_or_else(|| repo().join("mcu/result/zd1.hex"));
            let img = McuImage::parse(&mcu_path.display().to_string(), &read(&mcu_path)?)?;
            let system = read(&system.unwrap_or_else(|| repo().join("emu/img/os/system.cpio.gz")))?;
            let want = img.ident().map_or("unknown".into(), |i| i.version);
            println!(
                "install: {}MCU firmware {want} ({}), system {} ({} bytes)",
                bcd.as_ref().map_or(String::new(), |b| format!("boot image {} bytes, ", b.len())),
                mcu_path.display(),
                zd1_host::image::system_version(&system)?,
                system.len()
            );
            if !yes {
                println!("dry run: --yes to install");
                return Ok(0);
            }
            let dir = files::backup_dir(unit)?;
            term.show_log(true);
            let images = install::Images { bcd: bcd.as_deref(), mcu: &img, system: &system };
            install::install(bus, &images, ev.clone(), async |b| files::save_backup(&dir, b, &*ev)).await?;
            return Ok(0);
        }
        Cmd::Rollback { bcd, mcu, unit, yes } => {
            let bcd = read(&bcd)?;
            let img = McuImage::parse(&mcu.display().to_string(), &read(&mcu)?)?;
            println!("rollback: the module image {} bytes, MCU firmware {}", bcd.len(), mcu.display());
            if !yes {
                println!("dry run: --yes to put the stock firmware back on both boards");
                return Ok(0);
            }
            let dir = files::backup_dir(unit)?;
            install::rollback(bus, &bcd, &img, ev.clone(), async |b| files::save_backup(&dir, b, &*ev)).await?;
            return Ok(0);
        }
        Cmd::Update { image, no_start, baud, abort_after, uart } => {
            let image = read(&image.unwrap_or_else(|| repo().join("emu/img/os/system.cpio.gz")))?;
            if image.starts_with(b"bCoD") {
                let mut m = open(false).await?;
                // a module in BL2 with no image to start (a boot image update that stopped): BL2's XModem, no relay
                if m.call(&msg::Info).await?.mod_state == zd1_host::spec::ModState::Bl2 {
                    install::boot_image_in_bl2(&mut m, &image).await?;
                } else {
                    install::boot_image(bus, Some(m), &image, ev).await?;
                }
                return Ok(0);
            }
            if uart {
                return update_uart(bus, cli.wait, &image, baud, !no_start, ev).await;
            }
            term::catch_ctrl_c(); // Ctrl-C sends ABORT
            let mut m = open(false).await?;
            let o =
                update::Options { baud, start: !no_start, abort_after_ms: abort_after.map(|s| (s * 1000.0) as u32) };
            return Ok(match update::system(&mut m, &image, &o).await? {
                update::Outcome::FellBack { .. } => 1,
                _ => 0,
            });
        }
        _ => {}
    }
    let mut m = open(matches!(cli.cmd, Cmd::Log { .. })).await?;
    // what reports its progress in the MCU's log (the module's power-up, the source manager) shows it
    term.show_log(
        cli.log
            || matches!(
                cli.cmd,
                Cmd::Log { .. }
                    | Cmd::Play { .. }
                    | Cmd::Standby
                    | Cmd::Key { .. }
                    | Cmd::Module { .. }
                    | Cmd::Bootloader
                    | Cmd::Reboot
                    | Cmd::Amp { .. }
                    | Cmd::Debug { .. }
            ),
    );
    match cli.cmd {
        Cmd::Info => println!("{}", fmt::info(&m.call(&msg::Info).await?)),
        Cmd::Diag => println!("{}", fmt::diag(&m.call(&msg::Diag).await?)),
        Cmd::Hello => {
            let h = &m.hello;
            println!("{} ({}), board {}, protocol {}, up {} s", h.fw, h.built, h.board, h.proto, h.uptime_ms / 1000);
        }
        Cmd::Log { follow } => {
            while m.poll().await? > 0 {}
            if follow {
                term::catch_ctrl_c();
                m.follow().await?;
            }
        }
        Cmd::Play { what } => {
            m.call(&speaker::select(&what).expect("one of the choices")).await?;
            m.follow_for(300).await?; // its [src] / [wifi] lines
        }
        Cmd::Standby => {
            m.call(&speaker::select("standby").expect("a choice")).await?;
            m.follow_for(300).await?;
        }
        Cmd::Volume { step, bass } => {
            let i = m.call(&msg::Info).await?;
            if step.is_some() || bass.is_some() {
                m.call(&msg::Volume { volume: step.unwrap_or(i.volume), bass: bass.unwrap_or(i.bass) }).await?;
            } else {
                println!("volume {}/71, bass {}", i.volume, i.bass);
            }
        }
        Cmd::Key { key } => {
            m.call(&msg::Key { key }).await?;
            m.follow_for(300).await?; // what it did
        }
        Cmd::Amp { mode } => {
            // deny: also at boot (until `amp auto`)
            m.call(&msg::Amp { allow: !matches!(mode, AmpMode::Deny), keep: !matches!(mode, AmpMode::Allow) }).await?;
        }
        Cmd::Prefs { auto_switch, amp_idle, volume_cap } => {
            let bit = |f, on: bool| {
                if on { zd1_host::spec::message("Prefs").and_then(|m| m.mask_bit(f)).unwrap_or(0) } else { 0 }
            };
            let change = bit("auto_switch", auto_switch.is_some())
                | bit("amp_idle_s", amp_idle.is_some())
                | bit("volume_cap", volume_cap.is_some());
            let r = m
                .call(&msg::Prefs {
                    change,
                    auto_switch: auto_switch == Some(OnOff::On),
                    amp_idle_s: amp_idle.unwrap_or(0) * 60,
                    volume_cap: volume_cap.unwrap_or(0),
                })
                .await?;
            println!("{}", fmt::prefs(&r));
        }
        Cmd::Gate { action: None } => println!("{}", speaker::gate(&m.call(&msg::Info).await?)),
        Cmd::Gate { action: Some(GateAction::Unlock) } => {
            ev.prompt("press ON/OFF on the speaker within 30 s (its LED flashes purple)");
            m.call(&msg::Unlock).await?;
            println!("unlocked");
        }
        Cmd::Gate { action: Some(a @ (GateAction::Lock | GateAction::Open)) } => {
            let open = matches!(a, GateAction::Open);
            m.call(&msg::Lock { open }).await?;
            println!("gate {}", if open { "open for good" } else { "locked" });
        }
        Cmd::Settings { export: true, .. } => print!("{}", settings::export(&mut m).await?),
        Cmd::Settings { import: Some(f), .. } => {
            let n = settings::import(&mut m, &String::from_utf8_lossy(&read(&f)?)).await?;
            println!("{n} settings written");
        }
        Cmd::Settings { key: None, .. } => {
            for (k, v) in settings::list(&mut m).await? {
                println!("{k:<16} {v}");
            }
        }
        Cmd::Settings { key: Some(k), value: None, .. } => println!("{}", settings::get(&mut m, &k).await?),
        Cmd::Settings { key: Some(k), value: Some(v), .. } if zd1_host::spec::action(&k).is_some() => {
            settings::set(&mut m, &k, &v).await?;
            println!("{k}: done");
        }
        Cmd::Settings { key: Some(k), value: Some(v), .. } => {
            println!("{k} = {}", settings::set(&mut m, &k, &v).await?)
        }
        Cmd::Reset { all, yes } => {
            let what = if all {
                "every setting, the known Wi-Fi networks and AirPlay's pairings"
            } else {
                "every setting (the networks and pairings stay)"
            };
            if !yes {
                return Err(Error::Failed(format!("this puts {what} back to the defaults: again with --yes")));
            }
            settings::reset(&mut m, all).await?;
            println!("back to the defaults: {what}");
        }
        Cmd::Pairings { forget: true } => {
            settings::set(&mut m, "airplay.forget", "yes").await?;
            println!("AirPlay's pairings forgotten");
        }
        Cmd::Pairings { forget: false } => {
            let p = settings::pairings(&mut m).await?;
            if p.is_empty() {
                println!("no controller paired (pairing needs airplay.pin)");
            }
            for c in p {
                println!("{}{}", c.id, if c.admin { "  (admin)" } else { "" });
            }
        }
        Cmd::Servers => {
            let mut s = settings::servers(&mut m).await?;
            if s.is_empty() {
                zd1_host::sleep(2000).await;
                s = settings::servers(&mut m).await?;
            }
            if s.is_empty() {
                println!("no snapserver answered");
            }
            for x in s {
                println!("{:<32} {}:{}", x.name, x.host, x.port);
            }
        }
        Cmd::State => {
            let st = settings::state_msg(&mut m).await?;
            for (f, v) in st.fields() {
                println!("{:<16} {}{}", f.name, settings::shown(f, v), if f.set { "" } else { "  (status)" });
            }
        }
        Cmd::Name { name: None } => println!("{}", settings::get(&mut m, "name").await?),
        Cmd::Name { name: Some(n) } => println!("{}", settings::set(&mut m, "name", &n).await?),
        Cmd::Wifi { off: true, .. } => {
            settings::wifi_off(&mut m).await?;
            println!("Wi-Fi off");
        }
        Cmd::Wifi { scan: true, .. } => {
            // the module answers from its last scan and starts one: a fresh radio's first answer is empty
            let mut nets = settings::scan(&mut m).await?;
            if nets.is_empty() {
                println!("scanning...");
                zd1_host::sleep(5000).await;
                nets = settings::scan(&mut m).await?;
            }
            if nets.is_empty() {
                println!("no network seen");
            }
            for n in nets {
                println!("{:<32} -{:>3} dBm  {}", n.ssid, n.signal, fmt::security(n.security));
            }
        }
        Cmd::Wifi { known: true, .. } => {
            let n = settings::networks(&mut m).await?;
            if n.is_empty() {
                println!("no network known");
            }
            for (i, x) in n.iter().enumerate() {
                println!("{}. {:<32} {}", i + 1, x.ssid, fmt::security(x.security));
            }
        }
        Cmd::Wifi { ssid: Some(ssid), forget: true, .. } => {
            settings::forget(&mut m, &ssid).await?;
            println!("forgot {ssid}");
        }
        Cmd::Wifi { ssid: Some(ssid), first: true, .. } => {
            settings::prefer(&mut m, &ssid).await?;
            println!("{ssid} first");
        }
        Cmd::Wifi { ssid: None, .. } => {
            let st = settings::state(&mut m).await?;
            let known = settings::networks(&mut m).await.unwrap_or_default();
            println!("{}", speaker::wifi(&st, &known).0);
        }
        Cmd::Wifi { ssid: Some(ssid), open, passphrase_file, .. } => {
            let pw = match (&passphrase_file, open) {
                (_, true) => None,
                (Some(f), _) => Some(String::from_utf8_lossy(&read(f)?).lines().next().unwrap_or("").to_string()),
                (None, _) => Some(term::passphrase(&format!("passphrase for {ssid}: "))?),
            };
            let how = pw.as_deref().map_or(settings::Wifi::Open, settings::Wifi::Wpa2);
            settings::join(&mut m, &ssid, how).await?;
            println!("Wi-Fi: {ssid} ({}); the speaker joins it now", if open { "open" } else { "WPA2" });
        }
        Cmd::Ip { address, gateway, dns, eth } => {
            let i = if eth { "eth" } else { "wifi" };
            let mode = match address.as_str() {
                "off" | "dhcp" => address.clone(),
                _ => "static".into(),
            };
            if mode == "static" {
                settings::set(&mut m, &format!("{i}.address"), &address).await?;
                if let Some(g) = gateway {
                    settings::set(&mut m, &format!("{i}.gateway"), &g).await?;
                }
                if let Some(d) = dns {
                    settings::set(&mut m, &format!("{i}.dns"), &d).await?;
                }
            }
            println!("{i}.mode = {}", settings::set(&mut m, &format!("{i}.mode"), &mode).await?);
        }
        Cmd::Receiver { name, state } => {
            let key = name.to_possible_value().unwrap().get_name().to_string();
            let on = state.to_possible_value().unwrap().get_name().to_string();
            println!("{key} {}", settings::set(&mut m, &key, &on).await?);
        }
        Cmd::Slot { op: None, .. } => println!("{}", fmt::boot(&m.call(&msg::BootState).await?)),
        Cmd::Slot { op: Some(op), restart } => {
            let (op, slot) = match op {
                SlotOp::Try { slot } => (
                    BootOp::Try,
                    match slot {
                        TrySlot::A => Slot::A,
                        TrySlot::B => Slot::B,
                        TrySlot::Rescue => Slot::Rescue,
                    },
                ),
                SlotOp::Good { slot } => (
                    BootOp::Good,
                    match slot {
                        GoodSlot::A => Slot::A,
                        GoodSlot::B => Slot::B,
                    },
                ),
                SlotOp::Clear => (BootOp::Clear, Slot::None),
            };
            println!("{}", fmt::boot(&m.call(&msg::Boot { op, slot, restart }).await?));
        }
        Cmd::Module { op, timeout } => {
            let r = m.call(&msg::Module { op, timeout_s: timeout }).await?;
            println!("module {}, status 0x{:04x}{}", r.state, r.status, if r.ok { "" } else { " (FAILED)" });
            return Ok(if r.ok { 0 } else { 1 });
        }
        Cmd::Reg { op: RegOp::Read { reg, v } } => {
            let d = m.call(&msg::ModRead { reg, v }).await?.data;
            println!(
                "reg 0x{reg:02x}: {}  |{}|",
                d.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "),
                d.iter().map(|x| if (32..127).contains(x) { *x as char } else { '.' }).collect::<String>()
            );
        }
        Cmd::Reg { op: RegOp::Write { reg, data, v } } => {
            if !v && data.0.len() != 2 {
                return Err(Error::Failed("an F register takes two bytes (--v: a V payload)".into()));
            }
            m.call(&msg::ModWrite { reg, v, data: data.0 }).await?;
        }
        Cmd::Debug { mode } => {
            m.call(&msg::Debug { mode }).await?;
            println!("the module OS's debug services: {mode}");
        }
        Cmd::Peek { addr, len, words } => {
            let d = m.call(&msg::Peek { addr, len, words }).await?.data;
            for (i, row) in d.chunks(16).enumerate() {
                let cells: Vec<String> = if words {
                    row.chunks(4).map(zd1_host::hex).collect()
                } else {
                    row.iter().map(|x| format!("{x:02x}")).collect()
                };
                println!("{:08x}: {}", addr + 16 * i as u32, cells.join(" "));
            }
        }
        Cmd::Poke { addr, data, words } => {
            m.call(&msg::Poke { addr, data: data.0, words }).await?;
        }
        Cmd::Reboot => {
            m.call(&msg::Reboot).await?;
        }
        Cmd::Bootloader => {
            flash::enter_dfu(bus, Some(m), ev.clone()).await?;
        }
        Cmd::BulkTest { mb } => {
            let mut data = vec![0u8; (mb * 1048576.0) as usize];
            let mut x = zd1_host::seed() | 1; // xorshift: data the MCU can't guess
            for b in data.iter_mut() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = x as u8;
            }
            let (ok, line) = lab::bulk_test(&mut m, &data).await?;
            println!("{line}");
            return Ok(if ok { 0 } else { 1 });
        }
        Cmd::Console { action: None } => term::console(&mut m).await?,
        Cmd::Console { action: Some(ConsoleAction::Run { timeout, cmds }) } => {
            let mut sh = console::Shell::open(&mut m).await?;
            let mut status = 0;
            for c in &cmds {
                let (out, s) = sh.run(c, timeout * 1000).await?;
                if !out.is_empty() {
                    println!("{out}");
                }
                status = s;
            }
            sh.close().await?;
            return Ok(status);
        }
        Cmd::Console { action: Some(ConsoleAction::Log) } => {
            term::catch_ctrl_c();
            m.console_open(true).await?;
            let mut o = std::io::stdout();
            while !ev.cancelled() {
                if m.poll().await? == 0 {
                    m.usb.sleep(20).await;
                }
                use std::io::Write as _;
                let _ = o.write_all(&m.console_take()).and_then(|_| o.flush());
            }
            m.console_open(false).await?;
        }
        Cmd::Xmodem { image, in_bl2, blocks } => {
            for l in xmodem::xm_install(&mut m, &read(&image)?, in_bl2, blocks).await? {
                println!("{l}");
            }
        }
        Cmd::Dfu { .. }
        | Cmd::Flash { .. }
        | Cmd::Update { .. }
        | Cmd::Install { .. }
        | Cmd::Rollback { .. }
        | Cmd::Image { .. } => unreachable!(),
    }
    Ok(0)
}

async fn open_mcu<B: Bus>(bus: &B, wait: f64, ev: Rc<dyn Events>, backlog: bool) -> Result<Mcu<B::Dev>> {
    let mut waited = 0.0;
    loop {
        if let Some(m) = Mcu::open(bus, ev.clone(), backlog).await? {
            return Ok(m);
        }
        if waited >= wait {
            return Err(Error::Device(format!(
                "the speaker is not on USB as {:04X}:{:04X} (our MCU firmware); in DFU it is \
                                              03EB:2FF8",
                zd1_host::spec::usb::VID,
                zd1_host::spec::usb::PID
            )));
        }
        bus.sleep(300).await; // in the bus's time (the emulator's: its own)
        waited += 0.3;
    }
}

async fn update_uart<B: Bus>(
    bus: &B,
    wait: f64,
    image: &[u8],
    baud: u32,
    start: bool,
    ev: Rc<dyn Events>,
) -> Result<i32> {
    let addr = std::env::var("ZD1_MODCON_EMU")
        .map_err(|_| Error::Failed("--uart: the emulator's console (ZD1_MODCON_EMU)".into()))?;
    let mut s = std::net::TcpStream::connect(&addr).map_err(|e| Error::Device(format!("{addr}: {e}")))?;
    s.set_read_timeout(Some(std::time::Duration::from_millis(50))).ok();
    let slot = update::via_console(&mut s, image, baud, &*ev)?;
    if !start {
        return Ok(0);
    }
    if EmuBus::from_env().is_none() {
        println!("no MCU in this run: start it with the MCU (`zd1 slot try {slot} --restart`)");
        return Ok(0);
    }
    let mut m = open_mcu(bus, wait, ev, false).await?;
    Ok(match update::start_once(&mut m, slot).await? {
        update::Outcome::Running(_) => 0,
        _ => 1,
    })
}
