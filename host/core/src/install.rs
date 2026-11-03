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
use crate::spec::{ModOp, ModState, Slot};
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

fn ours_bcd(bcd: &[u8]) -> Result<()> {
    match crate::image::identify("", bcd)? {
        crate::image::Kind::Bcd { ours: true, .. } => Ok(()),
        _ => Err(Error::Failed("not our module boot image (a .bcd of ours)".into())),
    }
}

/// The module's boot image alone (the app partition: stub, kernel, rescue), on a speaker that runs our firmware:
/// into DFU, the relay, our firmware started again, the module up on its good slot. The system slots, `/data` and
/// the MCU firmware stay as they are. For a release whose boot image differs from the one the module started from
/// (the State record's `boot`, the .bcd's `zd1.boot=`). A relay that stops after BL2 erased the app partition leaves
/// the module nothing to start: our firmware is started again all the same, and a module that settles in BL2 gets
/// the image through BL2's XModem ([`boot_image_in_bl2`]); one up on its old boot image (nothing was erased) is the
/// relay's error.
pub async fn boot_image<B: Bus>(
    bus: &B,
    mcu: Option<Mcu<B::Dev>>,
    bcd: &[u8],
    ev: Rc<dyn Events>,
) -> Result<msg::InfoReply> {
    ours_bcd(bcd)?;
    let mut d = flash::enter_dfu(bus, mcu, ev.clone()).await?;
    let relayed = d.relay(bcd, &Relay::default(), &*ev).await;
    drop(d); // flash::start opens the bootloader again
    if let Err(e) = &relayed {
        ev.step(&format!("the relay stopped ({e}): our firmware again, then where the module is"));
    }
    flash::start(bus, ev.clone()).await?;
    let mut m = Mcu::open(bus, ev.clone(), false)
        .await?
        .ok_or_else(|| Error::Device("our firmware left USB right after it answered".into()))?;
    if let Err(e) = relayed {
        let i = settle(&mut m, FIRST_BOOT_MS).await?;
        if i.mod_state != ModState::Bl2 {
            return Err(Error::Failed(format!(
                "the relay stopped before the boot image was written ({e}); the module is {} as it was",
                i.mod_state
            )));
        }
        return boot_image_in_bl2(&mut m, bcd).await;
    }
    let i = wait_module(&mut m, FIRST_BOOT_MS).await?;
    ev.step(&format!("the module runs {}", i.mod_os));
    Ok(i)
}

/// The module's boot image written through BL2's XModem, our firmware driving it (`XmFrame`; ~5 min, no DFU: the
/// connection stays), for a module in BL2 with no image to start (an update of the boot image that stopped after BL2
/// erased the app partition: "Application: invalid image"). Then, once BL2 is done, the module powered up again and
/// up on its good slot; back in BL2 with no image, written once more.
pub async fn boot_image_in_bl2<U: crate::usb::Usb>(m: &mut Mcu<U>, bcd: &[u8]) -> Result<msg::InfoReply> {
    ours_bcd(bcd)?;
    let ev = m.events().clone();
    for attempt in 0..2 {
        ev.step("the module's boot image through BL2's XModem (about 5 minutes)");
        for l in crate::xmodem::xm_install(m, bcd, true, None).await? {
            ev.step(&l);
        }
        bl2_done(m).await?;
        m.call(&msg::Module { op: ModOp::Restart, timeout_s: 0 }).await?; // off, then the bring-up
        let i = settle(m, FIRST_BOOT_MS).await?;
        if i.mod_state == ModState::Bl2 && attempt == 0 {
            ev.step("the module is in BL2 again with no image to start: once more");
            continue;
        }
        let i = wait_module(m, FIRST_BOOT_MS).await?;
        ev.step(&format!("the module runs {}", i.mod_os));
        return Ok(i);
    }
    Err(Error::Failed("the module's boot image didn't take: BL2 has no image to start after two writes".into()))
}

/// BL2 acknowledges a download's EOT before it is done with it: it still writes the image, its console busy, and a
/// power-off then leaves the image invalid [HW 2026-11-03: the restart right after the EOT, BL2 then "Application:
/// invalid image"]. Done once its console (in the MCU's log) has been quiet for 10 s; 3 min at most.
async fn bl2_done<U: crate::usb::Usb>(m: &mut Mcu<U>) -> Result<()> {
    m.events().step("waiting for BL2 to finish writing (its console quiet)");
    let t0 = crate::now_ms();
    let mut last = t0;
    while crate::now_ms() - last < 10_000.0 && crate::now_ms() - t0 < 180_000.0 {
        m.usb.sleep(500).await;
        if m.poll().await? > 0 {
            last = crate::now_ms();
        }
    }
    Ok(())
}

/// The module up as our OS, speaking this firmware's protocol: a slot or the rescue, whichever its first boot ends
/// on. Anything else it settles on is the reason it can't go on.
async fn wait_module<U: crate::usb::Usb>(m: &mut Mcu<U>, ms: f64) -> Result<msg::InfoReply> {
    let i = settle(m, ms).await?;
    match i.mod_state {
        ModState::Ready => Ok(i),
        ModState::Stock => {
            Err(Error::Failed("the module runs the stock software: its boot image wasn't installed".into()))
        }
        ModState::Mismatch => Err(Error::Failed(format!(
            "the module's software speaks link protocol {}, the firmware just written {}: the boot image and the firmware \
             are from different releases",
            i.mod_proto, i.link_proto
        ))),
        ModState::Bl2 => Err(Error::Failed("the module is in BL2: its boot image doesn't start".into())),
        s => Err(Error::Failed(format!("the module settled as {s}"))),
    }
}

/// Where the module settles after a power-up: ready (our OS), the stock software, our OS with another link
/// protocol, or BL2 (no image to start). Off for 30 s (the MCU gave up on it) is an error, as is nothing settled in
/// `ms`.
async fn settle<U: crate::usb::Usb>(m: &mut Mcu<U>, ms: f64) -> Result<msg::InfoReply> {
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
            ModState::Ready | ModState::Stock | ModState::Mismatch | ModState::Bl2 => return Ok(i),
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
