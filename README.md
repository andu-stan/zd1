# zd1: new firmware for the B&W Zeppelin Air

<p align="center"><a href="https://pawlowskialex.github.io/zd1/"><img src="docs/img/page-speaker.png" width="600"
alt="The web page's drawing of the speaker: its outline lit white while it plays AirPlay, sound coming off both ends"></a></p>

The Bowers & Wilkins Zeppelin Air (2011, the one with the 30-pin iPod dock) is a very good speaker that B&W stopped
updating long ago: AirPlay 1 only, on a network module from 2010. This project replaces the
firmware on both of its boards, so the same speaker plays **AirPlay 2** (multi-room, in step with your other AirPlay
speakers) and **Snapcast**, and is set up and updated from a computer over USB. Nothing to open, solder or
take apart: everything goes in through the USB port on the back, and the stock firmware can go back on the same way.

The [web page](https://pawlowskialex.github.io/zd1/) installs it and then shows the speaker as it is, above: its
LED's colour and look as a glow on the outline (white for AirPlay, breathing when paused, dim with the amp asleep),
sound coming off both ends while it plays.

It is a hobby project, reverse-engineered from the stock firmware and developed on **one speaker**. Read
"Before you start" and "What doesn't work" below before you put it on yours.

## What you get
- **AirPlay 2** from iPhone, iPad, Mac and Apple TV: Music, the Mac's system audio, lossless (24-bit ALAC) from an
  Apple TV, groups with other AirPlay 2 speakers (in step with a Mac's speakers within a millisecond), volume and
  play/pause both ways, an optional PIN. **AirPlay 1** senders play too.
- **Snapcast**: the speaker joins a snapserver on your network as a client (PCM and FLAC).
- **USB audio** from a computer, as stock, and **AUX** (analog); the speaker picks the source that starts playing.
- The amp sleeps after 5 minutes of silence and wakes with sound; the LED shows the source and whether it plays.
- **Settings and updates over USB** from a web page (Chrome, Edge or Brave: WebUSB), or `zd1` on the command line. The
  name, Wi-Fi, receivers, volume limit and both boards' firmware; updates fall back to the previous version by
  themselves if the new one doesn't start.

The network module's own coprocessor decodes the audio (our AAC, ALAC, Vorbis and FLAC decoders), which leaves room
on a 2010 ARM9 for all of this. What works and how it was checked: `docs/status.md`.

## What doesn't work
- **The iPod dock.** The 30-pin dock does nothing with this firmware: no playback or control from a docked device.
- **Spotify Connect.** The speaker shows up in the Spotify app and logs in, but Spotify refused the audio key of every
  track to the one account it was tried with, also with upstream librespot on a computer
  (librespot-org/librespot#1649). It may work with another account; nobody has been able to check. Spotify plays on
  the speaker through AirPlay meanwhile.
- **Untested:** the IR remote (its decoder has run only in the emulator) and optical AUX (mini-TOSLINK). Analog AUX
  works.
- **Windows.** The web page and the tools run on macOS and Linux; Windows can't open the speaker yet (the firmware
  lacks WinUSB descriptors).
- **Setup from a phone.** B&W's app and its setup network are gone: Wi-Fi is set up from a computer over USB.
- **5 GHz and newer Wi-Fi security.** The radio is 802.11b/g, 2.4 GHz only. WPA2 networks (and WPA3 transition
  ones) join; WPA3-only, WEP and enterprise networks can't. Wired Ethernet works.
- **The Home app and HomeKit**, IPv6, and control over the network (settings and updates go over USB).

## Before you start: the risks
- **This rewrites the firmware of both boards.** It is built so that you can always get back: the tools refuse to
  write the MCU's bootloader and the network module's two bootloaders, every MCU flash takes a backup first, and `zd1
  rollback` puts the stock firmware back on both boards. Holding ON/OFF while plugging the speaker in starts B&W's
  own bootloader whatever the firmware does. Still: a failure at the wrong moment, or a fault nobody has met yet,
  could leave the speaker needing more work to recover (`docs/recovery/factory-reset.md`). The install from a stock
  speaker and the roll back have run on the development unit phase by phase, not yet as one flow, and the page's
  Install and Roll back only against the emulator: the first installs are the test.
- **The amp's power supply is the Zeppelin Air's weak spot.** On the development unit it failed once and was
  repaired. Our firmware keeps the stock protections, caps the volume (below, Living with it), backs the volume off when the board
  gets hot, and lets the amp sleep. `zd1 amp deny` locks the amp off, across restarts too.
- **AirPlay is reverse-engineered**, not Apple's: an iOS or macOS update could break something.
- **One unit tested**, a 30-pin Zeppelin Air. The **Zeppelin Air with Lightning connector** (2013) *might* work: B&W's
  MCU firmware for it is a near-twin build of the 2.04.05 ours follows (which also runs as its board variant), with the
  same network module (`docs/mcu/stock-firmware.md`). In the emulator our firmware and install path handle it; its MCU
  bootloader, which the install relies on, is unknown, nobody has tried one, and its dock is unsupported either way.
  The **Zeppelin A7** runs another MCU build on other hardware: not without changes.
- **On your network** the speaker runs an AirPlay receiver like any other AirPlay speaker. Its debug services
  (a shell and memory access) are off and can only be switched on over USB. Requests that change the speaker over
  USB (a flash, settings) need a press of ON/OFF on it first, so a web page you open can't change it on its own (below).
- No warranty (MIT license, below). If it breaks your speaker, you keep both pieces.

## Installing
**With the web page**: **https://pawlowskialex.github.io/zd1/**, in Chrome, Edge or Brave on a Mac or a Linux
computer (WebUSB), with a USB cable to the speaker's rear USB-B port. It installs, updates, configures and rolls back,
and needs nothing else on the computer.
1. Unplug the speaker, then hold ON/OFF while plugging it back in: the speaker starts B&W's bootloader, which the
   page can talk to whatever firmware is on it.
2. Open the page, **Connect**, pick the speaker, **Install**. It puts the network module's boot image on through the
   bootloader, then the MCU firmware (the page keeps a backup of what was there: a download and a copy in the
   browser), lets the module boot, then installs the system: about 5 minutes, each phase shown.
3. It lands on the network settings: join your Wi-Fi (the passphrase is turned into the key on your computer; only
   the key goes to the speaker), and rename the speaker if you like.

On Linux, the page shows a udev rule to copy first (your user's access to the USB device). The images are the
repository's releases (https://github.com/pawlowskialex/zd1/releases), built by GitHub Actions from the tagged
source; the page fetches them from the same site when it installs, and lists earlier releases to go back to.

**Updates**: connect, and Software offers the latest release; the system goes into the slot that doesn't run, and
the speaker goes back to the other one if the new one doesn't start.

**Back to stock**: in the bootloader view, **Roll back to B&W's software** (it takes B&W's images as files, the
ones in `blobs/` here). The speaker comes up as from the factory. **When something goes wrong**: hold ON/OFF while
plugging the speaker in, then connect: the page shows how far the last boot got and offers to install again or to
restore a backup it kept. More: `docs/recovery/factory-reset.md`.

**From the command line**: `zd1` (`host/cli`) does the same over USB and more. It runs from a checkout with
[nix](https://nixos.org/download/), which also builds the firmware yourself (on a Mac, with a Linux builder: the MCU's
compiler is an x86_64 Linux binary; `CONTRIBUTING.md`, Builds):
```
git clone https://github.com/pawlowskialex/zd1 && cd zd1
nix develop                                   # the tools, zd1 on the PATH
nix build -o mcu/result .#mcu                 # the MCU firmware
nix build -o emu/img/os .#os-image            # the network module's images
mkdir -p units/my-zeppelin                    # where the backups of your speaker go (kept out of git)
zd1 install --unit units/my-zeppelin --yes    # the speaker in its bootloader (step 1 above)
zd1 wifi "Your network"                       # asks for the passphrase
```
Then `zd1 info` shows the speaker's state, `zd1 --help` has the rest; `zd1 update` and `mcu/flash.sh` install new
builds, `zd1 rollback blobs/module/app_DMP.bcd blobs/mcu/zd1-2.04.05.hex --unit units/my-zeppelin --yes` goes back to
stock, `zd1 dfu restore BACKUP.tar --yes` writes a backup back, and `zd1 module esc` gets the network module into its
bootloader whatever it runs.

## Living with it
- **The volume is capped at step 40 of 71** by default, on every source (keys, remote, senders, the computer); the
  page's Behaviour (or `zd1 prefs --volume-cap N`) changes it.
- **The amp sleeps after 5 minutes of silence** and wakes with sound, so the first ~2.5 s after a long pause are
  lost while it comes back (Behaviour's "Off after", 1 to 30 minutes). The LED dims while it sleeps.
- **A computer that starts playing over USB takes the speaker** by itself after ~3 s (Behaviour turns that
  off); a sender picking the speaker over AirPlay takes it at once.
- **Changes over USB need a press.** A command or the web page that changes the speaker (a flash, its settings, the
  source or volume) asks for a press of ON/OFF first, then works until 10 minutes after the last change; the LED
  flashes purple while it waits. The page's Advanced (or `zd1 gate open`) drops that for good, for a speaker you
  drive from scripts.

## The project
Two boards: the **main board** with an Atmel AT32UC3A1256 MCU (AVR32: the DSP, codec, amp, keys, USB), and the
BridgeCo **CR870 network module** (DM870 SoC, ARM926EJ-S: Wi-Fi, Ethernet, audio out) that plugs into it. Our MCU
firmware (`mcu/`) replaces B&W's; on the module, our OS (`module/os/`) is a small Linux userspace in Rust on Nokia's
2.6.24 kernel for the same chip, with the receivers on our forks of shairplay-rust, librespot and libmdns. The CR870
is in other speakers of that era too, so the module side is written for any CR870 host. An emulator of both boards
(`emu/`, on Renode) runs it all for development.

- How it stands, with the evidence: `docs/status.md`; open work: `docs/backlog.md`; why things are as they are:
  `docs/decisions.md`; everything else: `docs/README.md`.
- Building, the layout, the rules for working on the hardware: `CONTRIBUTING.md`.

## License
Our code is MIT (`LICENSE`). Not ours and not under it: `blobs/` (B&W's, BridgeCo's and Nokia's binaries), `re/`
(material derived from the vendor firmware), `mcu/src/dsp_data.c` (B&W's DSP data), `mcu/asf/` and
`mcu/link_zd1.lds` (Microchip's ASF license), `mcu/dfp-include/` (Apache-2.0), `emu/renode/avr32/insn.decode` and the
decoder generated from it (LGPL-2.1). A few files are built from LGPL code (the AAC tables, the Warp0 ALAC decoder),
and the images hold GPL and LGPL programs: `NOTICE.md` lists each with its license and where its source is.

Not affiliated with Bowers & Wilkins, Apple or Spotify.
