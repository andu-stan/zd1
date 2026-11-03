//! The speaker as a whole (docs/web.md, Pairings): [`install`] puts our firmware on both boards, [`rollback`] puts the
//! stock firmware back. Both start from the MCU's DFU bootloader, reached from our firmware by itself (`Dfu`) or by
//! the key-hold power-up on a stock speaker, and both do the module first: the relay needs nothing from the MCU app,
//! and our MCU firmware over the stock module app (a state the firmware reports as STOCK, not a working speaker) is
//! what the order avoids. Each phase reads the state it needs before it starts and stops with the reason; nothing is
//! inferred from timing.

use crate::backup::Backup;
use crate::dfu::{self, Relay};
use crate::flash::{self, Report};
use crate::image::{McuImage, system_version};
use crate::spec::{ModState, Slot};
use crate::update::{self, Outcome};
use crate::usb::Bus;
use crate::{Error, Events, Mcu, Result, msg};
use std::rc::Rc;

/// What goes on: the module's boot image (`os.bcd`: the stub, the kernel, the rescue; None: it is there already, a
/// repair of the two others), our MCU firmware, the system image.
pub struct Images<'a> {
    pub bcd: Option<&'a [u8]>,
    pub mcu: &'a McuImage,
    pub system: &'a [u8],
}

/// How it ended: the firmware that runs, the slot the system went into (the good one now), its version.
#[derive(Debug, Clone)]
pub struct Installed {
    pub firmware: Report,
    pub slot: Slot,
    pub os: String,
}

/// How long the module's first boot may take: two empty slots failing (a panic restarts the module), then the
/// rescue, which the MCU gives 180 s.
const FIRST_BOOT_MS: f64 = 420_000.0;

/// Our firmware on both boards. `keep` gets the backup of the MCU's app region and user page before anything is
/// written to the MCU (the way back: `rollback` or a flash takes it as the MCU image).
pub async fn install<B: Bus>(
    bus: &B,
    img: &Images<'_>,
    ev: Rc<dyn Events>,
    keep: impl AsyncFnOnce(&Backup) -> Result<()>,
) -> Result<Installed> {
    let ident = img.mcu.ident().ok_or_else(|| {
        Error::Failed(
            "the MCU image carries no identification: not our firmware \
                                                              (back to stock is `rollback`)"
                .into(),
        )
    })?;
    let os = system_version(img.system)?;
    let phases = if img.bcd.is_some() { 4 } else { 3 };
    let mut n = 0;
    ev.step(&format!(
        "installing: MCU firmware {}, system {os}{}",
        ident.version,
        if img.bcd.is_some() { ", the module's boot image" } else { "" }
    ));
    let mut d = flash::enter_dfu(bus, None, ev.clone()).await?;
    if let Some(bcd) = img.bcd {
        n += 1;
        ev.phase(n, phases, "the module's boot image");
        d.relay(bcd, &Relay::default(), &*ev).await?;
    }
    n += 1;
    ev.phase(n, phases, "the MCU firmware");
    drop(d); // flash::firmware opens the bootloader again
    let firmware = flash::firmware(bus, None, img.mcu, ev.clone(), keep).await?;
    if !firmware.matches {
        return Err(Error::Failed(format!(
            "the firmware that runs is {}, not the {} just written",
            firmware.fw, ident.version
        )));
    }
    n += 1;
    ev.phase(n, phases, "the module's first boot");
    let mut m = Mcu::open(bus, ev.clone(), false)
        .await?
        .ok_or_else(|| Error::Device("the new firmware left USB right after it answered".into()))?;
    let i = wait_module(&mut m, FIRST_BOOT_MS).await?;
    ev.step(&format!("the module runs {}", i.mod_os));
    n += 1;
    ev.phase(n, phases, "the system");
    match update::system(&mut m, img.system, &update::Options::default()).await? {
        Outcome::Running(slot) => {
            ev.step(&format!("done: MCU firmware {}, system {os} in slot {slot}", firmware.fw));
            Ok(Installed { firmware, slot, os })
        }
        Outcome::FellBack { slot, now } => Err(Error::Failed(format!(
            "the system in slot {slot} didn't get ready: the module \
                                                                      is back on slot {now} (`zd1 log`, `zd1 slot`)"
        ))),
        Outcome::Installed(_) => unreachable!("started"),
    }
}

/// The module up as our OS, speaking this firmware's protocol: a slot or the rescue, whichever its first boot ends
/// on. Anything else it settles on is the reason it can't go on.
async fn wait_module<U: crate::usb::Usb>(m: &mut Mcu<U>, ms: f64) -> Result<msg::InfoReply> {
    let (t0, slow) = (crate::now_ms(), m.usb.slow() as f64);
    let late = |ms: f64| crate::now_ms() - t0 > ms * slow;
    let mut shown = None;
    loop {
        m.usb.sleep(2000).await;
        let _ = m.poll().await; // the MCU's log: the boot as it goes
        let i = match m.call(&msg::Info).await {
            Ok(i) => i,
            Err(e) if late(ms) => {
                return Err(Error::Timeout(format!("the module didn't get ready in {:.0} s ({e})", ms / 1000.0)));
            }
            Err(_) => continue, // the MCU busy with the module's power: again
        };
        if shown != Some(i.mod_state) {
            m.events().step(&format!("the module: {}", i.mod_state));
            shown = Some(i.mod_state);
        }
        m.events()
            .wait(&format!("the module's first boot ({})", i.mod_state), ((crate::now_ms() - t0) / 1000.0) as u64);
        match i.mod_state {
            ModState::Ready => return Ok(i),
            ModState::Stock => {
                return Err(Error::Failed(
                    "the module runs the stock software: its boot image wasn't installed".into(),
                ));
            }
            ModState::Mismatch => {
                return Err(Error::Failed(format!(
                    "the module's software speaks link protocol {}, the firmware just written {}: the boot image and the firmware \
                 are from different releases",
                    i.mod_proto, i.link_proto
                )));
            }
            ModState::Bl2 => return Err(Error::Failed("the module is in BL2: its boot image doesn't start".into())),
            ModState::Off if late(30_000.0) => {
                return Err(Error::Failed(
                    "the MCU powered the module off: nothing got ready (`zd1 log` has its console)".into(),
                ));
            }
            _ => {}
        }
        if late(ms) {
            return Err(Error::Timeout(format!(
                "the module didn't get ready in {:.0} s (its state: {})",
                ms / 1000.0,
                i.mod_state
            )));
        }
    }
}

/// The stock firmware back on both boards, the module first (the stock `.bcd` through the relay, ~6.5 min), then the
/// stock MCU firmware (or a backup of it), with a backup of what is on the MCU first.
/// The stock firmware has no host link: this is the last the program sees of the speaker. It comes up unconfigured,
/// as from the factory.
pub async fn rollback<B: Bus>(
    bus: &B,
    bcd: &[u8],
    mcu: &McuImage,
    ev: Rc<dyn Events>,
    keep: impl AsyncFnOnce(&Backup) -> Result<()>,
) -> Result<()> {
    if let Some(i) = mcu.ident() {
        return Err(Error::Failed(format!(
            "the MCU image is our firmware ({}), not stock: `install` puts ours on",
            i.version
        )));
    }
    let (start, end) = mcu.app_span()?;
    let mut d = flash::enter_dfu(bus, None, ev.clone()).await?;
    ev.phase(1, 2, "the stock module software");
    d.relay(bcd, &Relay::default(), &*ev).await?;
    ev.phase(2, 2, "the stock MCU firmware");
    let b = dfu::backup(&mut d, &*ev).await?;
    keep(&b).await?;
    if ev.cancelled() {
        return Err(Error::Cancelled);
    }
    if mcu.in_bootloader() > 0 {
        ev.step(&format!("({} bytes in the bootloader's region: left alone)", mcu.in_bootloader()));
    }
    dfu::write_verified(&mut d, start, &mcu.bytes(start, end), &*ev).await?;
    ev.step("starting the stock firmware: it has no host link, so this is the last this program sees of the speaker; it comes \
             up unconfigured, as from the factory");
    d.start(false).await?;
    Ok(())
}
