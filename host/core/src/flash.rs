//! Our MCU firmware onto the speaker without touching it: the host link's `Dfu` (or already in DFU) -> a backup of
//! what is on it (handed to the front-end to keep: the way back) -> erase, program, verify -> start -> the new
//! firmware answers, its version is the image's, and its cold-boot stage log. A stock speaker gets into DFU by the
//! key-hold power-on (ON/OFF held while plugging in), then the same from the backup on.

use crate::backup::Backup;
use crate::dfu::{self, Dfu};
use crate::image::McuImage;
use crate::spec::usb as link;
use crate::usb::{Bus, Iface, wait_open};
use crate::{Error, Events, Mcu, Result, msg};
use std::rc::Rc;

/// Where our firmware logs its cold boot: the last flash page, a big-endian word per stage, `B0 BOOT STAGE RCAUSE`
/// (mcu/src/main.c); 0xFFFFFFFF ends it. Readable from the firmware (`Peek`) and from DFU when it doesn't start.
pub const BOOT_LOG: u32 = 0x3_FE00;

/// A stage's name.
pub fn stage_name(s: u8) -> String {
    match s {
        1 => "entry",
        2 => "OSC0 ready",
        3 => "on OSC0",
        4 => "PLL locked",
        5 => "on PLL 48 MHz",
        6 => "GPIO+mux",
        7 => "udc_start",
        8 => "main loop",
        0x82 => "FAIL: OSC0 never ready",
        0x84 => "FAIL: PLL0 never locked",
        _ => return format!("{s:02x}"),
    }
    .into()
}

/// One logged stage: the boot it belongs to, the stage, the reset cause (PM RCAUSE: 1 power-on, 2 brown-out, 4 external,
/// 8 watchdog, 0x10 JTAG, 0x40 CPU error, 0x80 OCD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage {
    pub boot: u8,
    pub stage: u8,
    pub rcause: u8,
}

/// The boot log's entries (words that aren't log words are skipped).
pub fn boot_log(page: &[u8]) -> Vec<Stage> {
    page.as_chunks::<4>()
        .0
        .iter()
        .take_while(|w| **w != [0xFF; 4])
        .filter(|w| w[0] == 0xB0)
        .map(|w| Stage { boot: w[1], stage: w[2], rcause: w[3] })
        .collect()
}

/// The boot log, read through the bootloader (the firmware didn't start, or the speaker is in DFU).
pub async fn read_boot_log<U: crate::usb::Usb>(d: &mut Dfu<U>, ev: &dyn Events) -> Result<Vec<Stage>> {
    Ok(boot_log(&d.read(BOOT_LOG, crate::image::FLASH_SIZE, dfu::MEM_FLASH, ev).await?))
}

/// What the new firmware said.
#[derive(Debug, Clone)]
pub struct Report {
    pub fw: String,
    pub built: String,
    pub rcause: u8,
    pub stages: Vec<String>,
    pub matches: bool,
}

/// Into DFU: from our firmware through `Dfu` (`mcu`: a connection already open, else one is opened), or the bootloader
/// is there already. The bootloader, opened.
pub async fn enter_dfu<B: Bus>(bus: &B, mcu: Option<Mcu<B::Dev>>, ev: Rc<dyn Events>) -> Result<Dfu<B::Dev>> {
    let mcu = match mcu {
        Some(m) => Some(m),
        None => {
            if let Some(d) = bus.open(dfu::VID, dfu::PID, Iface::Number(dfu::IFACE)).await? {
                ev.step("already in DFU");
                return Ok(Dfu::new(d));
            }
            Mcu::open(bus, ev.clone(), false).await?
        }
    };
    match mcu {
        Some(mut m) => {
            ev.step(&format!("running {}: into DFU", m.hello.fw));
            m.call(&msg::Dfu).await?;
        }
        None => {
            return Err(Error::Device(
                "the speaker is not on USB (neither DFU 03EB:2FF8 nor our firmware 03EB:2FE0): \
                                          hold ON/OFF while powering it up, for DFU"
                    .into(),
            ));
        }
    }
    let t0 = crate::now_ms();
    let d =
        wait_open(bus, dfu::VID, dfu::PID, Iface::Number(dfu::IFACE), 15_000, &*ev, "the speaker's bootloader (DFU)")
            .await?
            .ok_or_else(|| {
                Error::Device(
                    "DFU did not appear within 15 s (power the speaker up with ON/OFF held, then again)".into(),
                )
            })?;
    ev.step(&format!("in DFU after {:.1} s", (crate::now_ms() - t0) / 1000.0));
    Ok(Dfu::new(d))
}

/// Flash `img`. `keep` gets the backup before anything is written (awaited: a front-end saving it); an error from it
/// stops the flash. `mcu`: our firmware's connection if one is open (it ends here).
pub async fn firmware<B: Bus>(
    bus: &B,
    mcu: Option<Mcu<B::Dev>>,
    img: &McuImage,
    ev: Rc<dyn Events>,
    keep: impl AsyncFnOnce(&Backup) -> Result<()>,
) -> Result<Report> {
    let (start, end) = img.app_span()?;
    let want = img.ident().map(|i| i.version);
    ev.step(&format!(
        "image: app bytes 0x{start:06x}..0x{end:06x}, {}",
        want.as_deref().unwrap_or("no identification")
    ));
    if img.in_bootloader() > 0 {
        ev.step(&format!("({} bytes in the bootloader's region: left alone)", img.in_bootloader()));
    }
    let mut d = enter_dfu(bus, mcu, ev.clone()).await?;
    let b = dfu::backup(&mut d, &*ev).await?;
    keep(&b).await?;
    if ev.cancelled() {
        return Err(Error::Cancelled);
    }
    dfu::write_verified(&mut d, start, &img.bytes(start, end), &*ev).await?;
    ev.step("starting the firmware");
    d.start(false).await?;
    drop(d);
    let r = check(bus, ev.clone(), want.as_deref()).await?;
    Ok(r)
}

/// From DFU into the firmware on the speaker, and what it says (`check`).
pub async fn start<B: Bus>(bus: &B, ev: Rc<dyn Events>) -> Result<Report> {
    let mut d = bus
        .open(dfu::VID, dfu::PID, Iface::Number(dfu::IFACE))
        .await?
        .map(Dfu::new)
        .ok_or_else(|| Error::Device("the speaker's bootloader (DFU 03EB:2FF8) is not on USB".into()))?;
    ev.step("starting the firmware");
    d.start(false).await?;
    drop(d);
    check(bus, ev, None).await
}

/// Wait for our firmware after a start, and read what it says: the version, the reset cause, the boot stages.
pub async fn check<B: Bus>(bus: &B, ev: Rc<dyn Events>, want: Option<&str>) -> Result<Report> {
    ev.step("waiting for the new firmware");
    let dev = wait_open(bus, link::VID, link::PID, crate::mcu::IFACE, 15_000, &*ev, "the speaker (our firmware)")
        .await?
        .ok_or_else(|| {
            Error::Device(
                "flashed and started, but it didn't answer within 15 s; it can still be flashed \
                                      from DFU (ON/OFF held at power-up)"
                    .into(),
            )
        })?;
    let mut m = Mcu::new(dev, ev.clone(), false).await?;
    let i = m.call(&msg::Info).await?;
    let words = m.call(&msg::Peek { addr: crate::image::FLASH_BASE + BOOT_LOG, len: 64, words: false }).await?;
    let stages = boot_log(&words.data).iter().map(|s| stage_name(s.stage)).collect();
    let r = Report { matches: want.is_none_or(|w| w == i.fw), fw: i.fw, built: i.built, rcause: i.rcause, stages };
    ev.step(&format!(
        "running {} ({}), reset cause 0x{:02x}{}",
        r.fw,
        r.built,
        r.rcause,
        if r.matches { "" } else { ": NOT the image just flashed" }
    ));
    ev.step(&format!("boot log: {}", r.stages.join(" > ")));
    Ok(r)
}
