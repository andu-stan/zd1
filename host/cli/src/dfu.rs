//! `zd1 flash` and `zd1 dfu`: our MCU firmware onto the speaker, and the B&W DFU bootloader itself (host/core's
//! `flash` and `dfu`).

use crate::files::{self, read};
use crate::number;
use clap::Subcommand;
use std::path::PathBuf;
use std::rc::Rc;
use zd1_host::backup::Backup;
use zd1_host::dfu::{self, Dfu};
use zd1_host::image::{FLASH_BASE, FLASH_SIZE, McuImage};
use zd1_host::usb::{Bus, Iface};
use zd1_host::{Error, Events, Result, flash};

#[derive(Subcommand)]
pub enum DfuCmd {
    /// The bootloader's state and the module's, as it sees it.
    Info,
    /// The app region and the user page as a backup (a tar: app_flash.bin, user_page.bin, manifest.json) into OUT,
    /// a file or a directory [default: the unit's mcu/dfu-backups/].
    Backup {
        out: Option<PathBuf>,
        #[arg(long)]
        unit: Option<PathBuf>,
    },
    /// Erase, program and verify an image (dry run without --yes); refused without a backup.
    Write {
        image: PathBuf,
        /// The backup of what is on it now (`zd1 dfu backup`).
        #[arg(long)]
        backup: PathBuf,
        /// A raw image's load address [default: 0x80004000, the app region].
        #[arg(long, value_parser = number::<u32>)]
        base: Option<u32>,
        #[arg(long)]
        yes: bool,
        /// Start the app afterwards.
        #[arg(long)]
        start: bool,
    },
    /// Write a backup's app region back (dry run without --yes).
    Restore {
        backup: PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        start: bool,
    },
    /// Program and verify the 512-byte user page (its ISP word at 0x1FC must be the known-good one, or --force).
    UserPage {
        image: PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        force: bool,
    },
    /// Read flash [START, END) (offsets or 0x8000.. addresses; the bootloader's region reads too) into OUT.
    Dump {
        #[arg(value_parser = number::<u32>)]
        start: u32,
        #[arg(value_parser = number::<u32>)]
        end: u32,
        out: PathBuf,
    },
    /// Relay a module image (.bcd) through the bootloader to the network module's BL2. BL2 erases the app partition
    /// as the download starts: once streaming, a stop leaves it invalid.
    Relay {
        image: PathBuf,
        /// Into BL2's own section (a protected region: the way back if the app section fails).
        #[arg(long)]
        bl2: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value_t = 5)]
        t1: u16,
        #[arg(long, default_value_t = 0)]
        t2: u16,
        /// A measurement: stream N blocks, stop without the end of transfer (LEAVES THE APP PARTITION INVALID).
        #[arg(long)]
        max_blocks: Option<usize>,
        /// Stop at BL2-ready without streaming (the emulator's fast path).
        #[arg(long)]
        handover_only: bool,
    },
    /// Our firmware's cold-boot log (the last flash page): how far its last boots got, when it doesn't start.
    BootLog,
    /// Leave DFU: run the app (or BL2).
    Start {
        #[arg(long)]
        bl2: bool,
    },
}

/// `zd1 flash`: the image (this checkout's build unless said), a backup into the unit's directory first.
pub async fn mcu_flash<B: Bus>(
    bus: &B,
    image: Option<PathBuf>,
    unit: Option<PathBuf>,
    ev: Rc<dyn Events>,
) -> Result<i32> {
    let path = image.unwrap_or_else(|| files::repo().join("mcu/result/zd1.hex"));
    let img = McuImage::parse(&path.display().to_string(), &read(&path)?)?;
    let want = img.ident().map_or("unknown".into(), |i| i.version);
    let dir = files::backup_dir(unit)?;
    ev.step(&format!("image {}: {want}", path.display()));
    let r = flash::firmware(bus, None, &img, ev.clone(), async |b| files::save_backup(&dir, b, &*ev)).await?;
    Ok(if r.matches { 0 } else { 1 })
}

async fn open_dfu<B: Bus>(bus: &B) -> Result<Dfu<B::Dev>> {
    bus.open(dfu::VID, dfu::PID, Iface::Number(dfu::IFACE))
        .await?
        .map(Dfu::new)
        .ok_or_else(|| Error::Device("no DFU device 03EB:2FF8 (`zd1 bootloader`, or ON/OFF held at power-up)".into()))
}

pub async fn dfu_cmd<B: Bus>(bus: &B, cmd: DfuCmd, events: Rc<dyn Events>) -> Result<i32> {
    let ev = &*events;
    match cmd {
        DfuCmd::Info => {
            let d = open_dfu(bus).await?;
            d.make_idle().await?;
            let s = d.status().await?;
            println!(
                "DFU idle; status {}, state {}, poll {} ms; the module: 0x{:02x} {}",
                s.status,
                s.state,
                s.poll_ms,
                s.coproc,
                dfu::coproc_status_name(s.coproc)
            );
        }
        DfuCmd::Backup { out, unit } => {
            let to = match out {
                Some(o) => o,
                None => files::backup_dir(unit)?,
            };
            let b = dfu::backup(&mut open_dfu(bus).await?, ev).await?;
            files::save_backup(&to, &b, ev)?;
        }
        DfuCmd::Write { image, backup, base, yes, start } => {
            let b = Backup::from_tar(&read(&backup)?).map_err(|e| {
                Error::Failed(format!("refusing to write: {}: {e} (`zd1 dfu backup` first)", backup.display()))
            })?;
            println!("the way back: {} holds {} ({})", backup.display(), b.holds(), b.when);
            let data = read(&image)?;
            let img = match base {
                Some(b) => McuImage::from_bin(b, &data),
                None => McuImage::parse(&image.display().to_string(), &data)?,
            };
            write_image(bus, &img, yes, start, ev).await?;
        }
        DfuCmd::Restore { backup, yes, start } => {
            let b = Backup::from_tar(&read(&backup)?)?;
            println!("{}: {}, read {}", backup.display(), b.holds(), b.when);
            let img = McuImage::parse("backup.tar", &read(&backup)?)?;
            write_image(bus, &img, yes, start, ev).await?;
        }
        DfuCmd::UserPage { image, yes, force } => {
            let data = read(&image)?;
            if !yes {
                println!("dry run: --yes to program and verify the user page");
                return Ok(0);
            }
            open_dfu(bus).await?.write_user_page(&data, force, ev).await?;
            println!("user page verified (sha {})", &zd1_host::hex(&zd1_host::sha256(&data))[..16]);
        }
        DfuCmd::Dump { start, end, out } => {
            let off = |v: u32| if v >= FLASH_BASE { v - FLASH_BASE } else { v };
            let (s, e) = (off(start), off(end));
            if e > FLASH_SIZE || s >= e {
                return Err(Error::Failed("START < END, within the flash".into()));
            }
            let data = open_dfu(bus).await?.read(s, e, dfu::MEM_FLASH, ev).await?;
            std::fs::write(&out, &data).map_err(|e| Error::Failed(format!("{}: {e}", out.display())))?;
            println!(
                "{}: {} bytes, {} not 0xFF, sha {}",
                out.display(),
                data.len(),
                data.iter().filter(|b| **b != 0xFF).count(),
                &zd1_host::hex(&zd1_host::sha256(&data))[..16]
            );
        }
        DfuCmd::Relay { image, bl2, yes, t1, t2, max_blocks, handover_only } => {
            let data = read(&image)?;
            println!(
                "{}: {} bytes -> the module's {} section",
                image.display(),
                data.len(),
                if bl2 { "BL2" } else { "app" }
            );
            if !yes {
                println!("dry run: --yes to relay it to the module");
                return Ok(0);
            }
            let o = dfu::Relay { bl2, t1, t2, max_blocks, handover_only, ..Default::default() };
            open_dfu(bus).await?.relay(&data, &o, ev).await?;
        }
        DfuCmd::BootLog => {
            let log = flash::read_boot_log(&mut open_dfu(bus).await?, ev).await?;
            let causes = [
                (0x01, "power-on"),
                (0x02, "brown-out"),
                (0x04, "external"),
                (0x08, "watchdog"),
                (0x10, "JTAG"),
                (0x40, "CPU error"),
                (0x80, "OCD"),
            ];
            for s in &log {
                let rc: Vec<&str> = causes.iter().filter(|(b, _)| s.rcause & b != 0).map(|c| c.1).collect();
                println!(
                    "boot {:2}  stage 0x{:02x} {:<26} reset cause {}",
                    s.boot,
                    s.stage,
                    flash::stage_name(s.stage),
                    rc.join("+")
                );
            }
            if log.is_empty() {
                println!("empty: the firmware never reached its first stage (or a flash erased the page)");
            }
        }
        DfuCmd::Start { bl2: false } => {
            flash::start(bus, events.clone()).await?;
        }
        DfuCmd::Start { bl2: true } => {
            open_dfu(bus).await?.start(true).await?;
            println!("started BL2");
        }
    }
    Ok(0)
}

async fn write_image<B: Bus>(bus: &B, img: &McuImage, yes: bool, start: bool, ev: &dyn Events) -> Result<()> {
    let (s, e) = img.app_span()?;
    println!(
        "app bytes 0x{s:06x}..0x{e:06x}{}",
        img.ident().map_or(String::new(), |i| format!(", {} ({})", i.version, i.built))
    );
    if img.in_bootloader() > 0 {
        println!("  ({} bytes in the protected bootloader region: left alone)", img.in_bootloader());
    }
    if !yes {
        println!("dry run: --yes to erase, program and verify");
        return Ok(());
    }
    let mut d = open_dfu(bus).await?;
    dfu::write_verified(&mut d, s, &img.bytes(s, e), ev).await?;
    if start {
        ev.step("starting the application");
        d.start(false).await?;
    }
    Ok(())
}
