# Status

What has been seen working on the unit, what has only run in the emulator, what is open and what is broken. The topic
docs carry the detail and the evidence; this page points at them. As of 2026-11-02. What is installed on a unit now
(versions, slots, settings) is per-unit: `units/<unit>/status.md`, not in git.

A result counts once it was seen on the unit. An emulator run says the code does something plausible, nothing more
(`emu/README.md`, "What the emulator proves"); where the two disagree, the unit is right.

## Works on the unit

### Main board (our MCU firmware)
| What | Seen | Detail |
|---|---|---|
| Cold boot, self-DFU (`Dfu`), `mcu/flash.sh` (backup, flash, verify, start); recovery by key-hold DFU and `dfu restore` | 2026-10-16 on, every flash since | `docs/mcu/bootloader-dfu.md` |
| `zd1 install --no-bcd` as one flow: into DFU, the backup, the MCU firmware, the module's boot watched to ready, the system into the other slot and confirmed (the relay phase not in this run) | 2026-10-31 | `docs/module/updater.md`, `host/core/src/install.rs` |
| `zd1 rollback` as one flow from our firmware: into DFU by itself, the stock `.bcd` through the relay (286 s), a backup of the MCU, the stock MCU firmware written, verified and started (on USB as B&W's own 19B5:0005, 2.04); whether the stock module app comes up after it is to be seen on the speaker | 2026-11-03 | `docs/module/updater.md` |
| The module states (ready / stock / mismatch / BL2) and the State record (`zd1 state`, `zd1 settings`: reg 0xD6, kept by the MCU, read by `ModRecord`; a setting written by its key, refused with the spec's reason) | 2026-10-31 | `docs/module/settings.md` |
| A newer tool (host protocol 2) put the protocol 1 firmware into DFU and installed over it: the host link's base layer (HELLO, `Dfu`) | 2026-10-31 | `docs/protocol/host.md`, Compatibility |
| V writes to our OS acknowledged by the module's echo, sent again without one; an `Update` started the moment the module is ready gets through (6 of 6 rounds with the slots' stamps recorded, at most one retry; an unacknowledged BEGIN fails at once and leaves the other slot as it was) | 2026-10-31, 2026-11-02 | `docs/protocol/sequences.md`, Our OS, point 6; `docs/module/storage.md` |
| Audio bring-up (CS2000, DSP download, codec), the amp sequence, stock's protections armed; no trip in 15 min amp-on | 2026-10-18 | `docs/hardware/zd1-main-board.md` |
| Sources USB, AUX analog, Wi-Fi (the module running our OS); ON/OFF stepping, standby, volume keys; settings kept across a power cycle | 2026-10-18, 2026-10-27 | `mcu/README.md`, Behaviour |
| USB audio: UAC1 48 / 44.1 / 32 kHz, no under- or overruns; HID media keys | 2026-10-18 | `mcu/README.md`, Design notes |
| The module's console over the host link (MCU d865f5e): `zd1 console run` (output and exit status; 109 KB identical in ~13 s, none lost), the terminal (Ctrl-C to the shell), `zd1 module esc` into BL2 (BL1's menu driven by the MCU; BL2's menu answering over the console) and `module restart` back, the page's console pane (Advanced); again on feffd90 without the CDC port | 2026-11-02 | `docs/protocol/host.md`, `mcu/README.md` |
| USB audio's status interrupt on macOS (MCU feffd90, the CDC port gone: IF0-IF3): the Mac's volume for the speaker follows every change made on the speaker, `zd1 volume` and the volume keys (steps 10, 11, 12, 11, 20 -> -44, -42, -40, -42, -26 dB); the Mac's own changes come in as before | 2026-11-02 | `mcu/README.md`, Design notes |
| The host link on macOS: `info`, `diag`, `log`, the gate (`Unlock` by a press, `gate open`), `Module` OFF / POWER / START / HOLD | 2026-10-26 | `docs/protocol/host.md` |
| The gate locked (requests that change the speaker refused until a press) and the interface claim (a second program refused); the debug services switched by the MCU (`zd1 debug on\|off`) | 2026-11-02 | `docs/protocol/host.md`, The gate; `module/os/README.md` |
| The bulk pipe (vendor interface, bulk OUT 0x05): 930 KB/s, CRC-32 right from 64 B to 16 MB; ~810 KB/s while USB audio streams and switches | 2026-10-26 | `docs/module/updater.md` |
| Enumerates as "Zeppelin Air" by Bowers & Wilkins (stock's strings, serial "ZD1") | 2026-10-26 | `mcu/README.md`, Behaviour |
| The behaviour on top of stock's, with the amp allowed: the amp asleep after a boot onto a silent source and woken by sound, off after 5 min of silence; picks answered at once (the speaker chosen on an iPhone for AirPlay, as the Mac's output, a cable into AUX: LED pulses, the amp up, nothing played until play); ON/OFF stepping between the connected sources (USB -> AirPlay, the song resuming); settling back to a playing AUX 30 s after a choice by hand | 2026-10-26/27 | `mcu/README.md`, Behaviour, Intents |
| The DSP's input meter (param 0x031): floor -86 dBFS with the source paused, music -66..-28 dBFS, before the volume stage | 2026-10-27 | `docs/mcu/stock-firmware.md`, DSP |
| The LED: the palette fitted to this LED as a camera measured it; its looks (a colour per source and receiver, breathing while silent, the crossfades) by eye | 2026-10-27, 2026-11-02 | `mcu/README.md`, The LED; `docs/hardware/zd1-main-board.md` |
| The MCU after its cleanup (dead code, shared helpers, one soft start): a source switch, and a wake with the supply still held, unmute at once without a click; a held volume key repeats | 2026-11-02 | `mcu/README.md`, Behaviour |
| `zd1 prefs`: the amp's idle time (off a minute after the sound with `--amp-idle 1`), the volume cap holding on every source (keys, the remote, AirPlay at 100 %, the computer); the computer's volume spanning the whole cap; no false pick when the Mac wakes with the speaker as its output | 2026-11-02 | `mcu/README.md`, Behaviour |
| The heat soft limit on ADC4 and ADC5 (caps the volume), both logged every 10 min of amp-on; ADC4's trip after 9 samples like stock | in the firmware since 2026-10-27; the cap has not been reached | `docs/hardware/zd1-main-board.md`, Protections |
| The native link to our OS (`docs/protocol/sequences.md`, Our OS): the hello at boot and after `zd1-linkd` restarts, a sender's volume onto 0..the cap (40 % -> step 16, 100 % -> step 40), the knob to the sender's slider, the keys as presses, play/pause as the sender's toggle; senders start from the speaker's volume, also right after a boot. Our OS from before the native registers gets the stock dialect and works | 2026-10-28 | `docs/protocol/sequences.md` |
| `Module POWER` from the main loop: the module's console comes through while it runs, the reply when it is ready (BL2 likewise: [EMU] only) | 2026-10-31, MCU 5f66c37 | `docs/protocol/host.md`, Module |
| The stock module app with our firmware: AirPlay 1, `zd1 module bl2` (HCR 0x21), the shell on telnet (register writes) | 2026-10-16 on | `docs/module/knos.md` |
| The stock MCU firmware with the telnet patch (`stock-patch/`), flashed and checked; not on the unit now | 2026-10-16 | `stock-patch/README.md` |

### Module: boot, recovery, updates
| What | Seen | Detail |
|---|---|---|
| BL2's web updater installs stock and our images; BL1 captured decrypted from SRAM | 2026-10-19 on | `docs/module/boot-chain.md` |
| Into BL2 whatever the image does: `Module ESC` (BL1's serial menu, the MCU driving it; 2026-11-02), HCR 0x21 (`zd1 module bl2`, against the stock app, the boot stub and `zd1-linkd`), JTAG (mailbox + reset pulse) | 2026-10-23, 2026-10-26 | `docs/module/boot-chain.md`, Getting into BL2 |
| The MCU bootloader's relay (`zd1 dfu relay`): a 9 MB image in 382 s, `os.bcd` (2 MB) in 85 s; its handover to BL2 against our OS (2e20f8d) 33.8 s after coproc-init | 2026-10-26, 2026-10-27 | `docs/mcu/bootloader-dfu.md` |
| Our MCU app as BL2's XModem sender (`zd1 xmodem`): 9.5 KB/s, 0 retries | 2026-10-26 | `docs/mcu/bootloader-dfu.md` |
| The system slots: the stub asks our MCU (reg 0xC4) which slot to boot, an empty slot fails once (panic, `panic=10`), the rescue starts when no slot gets ready, a slot started on trial is confirmed by its ready | 2026-10-26 | `docs/module/storage.md` |
| `zd1 update`: the system over the bulk pipe, the MCU and the module's UART into the slot that doesn't run, SHA-256 and stamp checked, started once and kept: 5 MB in 82 s framed at 1 Mbaud; an aborted or failed update leaves both boards running | 2026-10-26, every update since | `docs/module/updater.md` |
| `reboot` under our OS (timer 0 reset) comes back through BL1 into our OS | 2026-10-23 | `docs/module/memory-map.md`, timers |
| JTAG (J-Link on the carrier's pads): halt, registers, memory, the kernel log from RAM | 2026-10-23 | `docs/module/jtag.md` |

### Module: our OS
| What | Seen | Detail |
|---|---|---|
| Boots from NAND through BL1 and the stock preloader; `zd1-board up` sets what the Nokia kernel leaves unset (the pad matrix, the PHY's reset and clock, the clock gates); `zd1-linkd` brings the MCU to READY; `[os] up` ~16-22 s after power-on | 2026-10-23 on | `docs/module/linux.md` |
| The system as a JFFS2 root in its slot: 4.1 MB used, 24.3 MB of RAM available with every service running; `/data` kept across installs | 2026-10-26 | `docs/module/storage.md` |
| Settings from the Mac through the MCU (`zd1 settings`): Wi-Fi joined from them within ~3 s, a rename reaching the MCU and every receiver | 2026-10-26 | `docs/module/settings.md` |
| The records (`zd1 servers`, `zd1 pairings` and forgetting them) and the receivers' options (`spotify.bitrate`, `snapcast.latency`: each receiver restarted once with them) | 2026-11-02 | `docs/module/settings.md` |
| The web page against the unit (Brave on the Mac): every view, the State record's lines, the drawn speaker's glow following the LED | 2026-11-02 | `docs/web.md` |
| Ethernet 100 Mb/s: 169.254.1.1 at once, DHCP in the background; eth0 = MAC0, wlan0 = MAC0 locally administered (from PersParam, read raw) | 2026-10-23 | `docs/module/linux.md`, The MAC |
| Wi-Fi: WPA2-PSK through `wpa_supplicant` (`-Dwext`), a DHCP lease, on the radio's second antenna port (the AP hears the module at -56 dBm); ~0.7-1 % of downlink packets lost | 2026-10-24/25 | `docs/module/memory-map.md`, Wi-Fi |
| The wall clock by NTP over the Wi-Fi lease (init's `time`: the module boots in 1970, TLS needs the date) | 2026-10-25 | `module/os/README.md`, Init |
| Audio out through Nokia's `snd-sulu` (AV port 2 at 512·fs MCLK, like KnOS), heard through the speaker; the whole ALSA path ~1.8 % of the ARM with few wake-ups | 2026-10-24 on | `docs/module/linux.md`, `docs/module/memory-map.md` |
| The module's own auth chip on its I2C bus (an Authentication Coprocessor 2.0B) | 2026-10-25 | `docs/hardware/cr870-module.md` |

### Receivers
| What | Seen | Detail |
|---|---|---|
| **AirPlay 2** from a Mac (Music, the system's audio) and an iPhone, over the wired link and over Wi-Fi: realtime ALAC (decrypted and decoded on Warp0) and buffered AAC (our decoder on Warp0); 48 kHz at the right pitch; buffered ALAC/48000/S24 from a test sender | 2026-10-24 on | `docs/streaming.md` |
| AirPlay 2 in step with a Mac's own speakers in a group, within 0.6 ms (wired and Wi-Fi), through pause, seek and track changes; seeks and track changes faded, no xruns; the Mac's system audio 42.5 ms after the latency macOS reports | 2026-10-28 | `docs/streaming.md`, Synchronised playback |
| AirPlay 2 in a group with a MacBook and an Apple TV, in step by ear | before 2026-10-29 | `docs/streaming.md`, Synchronised playback |
| AirPlay 2 from an Apple TV as the sender: YouTube (AAC at 48 and 44.1 kHz, one video after another), Apple Music radio, lossless ALAC/48000/S24 (Home Sharing) without a gap; the installed receiver offers AAC-LC/48000 (system a1b290d on) | 2026-10-30 | `docs/streaming.md`, Formats |
| 24-bit AirPlay: an Apple TV's lossless ALAC/48000/S24 decoded on Warp0 and kept 24-bit to the DSP (two Apple Music lossless tracks; Apple's encoder's frames bit for bit from `ap2send`, the receiver ~15 % of the ARM) | 2026-10-30, system e674e93 | `docs/streaming.md`, Formats |
| AirPlay's play times on CLOCK_MONOTONIC: a step of the wall clock by an hour each way under a stream drops nothing (the boot's NTP step made a stream already playing late); in step with `ap2send`'s timeline at 44.1 and 48 kHz (45.4 / 45.0 ms with the microphone path) | 2026-10-30, system 515c689 | `docs/streaming.md`, The local clock |
| AirPlay 2 sessions back to back from a sender whose PTP master restarted under the same identity (its sequence numbers from 0: every `ap2send` run), at 44.1 and 48 kHz: nothing late (a Sync pairs only with a Follow_Up within a second; before, 151 frames late in a 30 s stream) | 2026-10-30, shairplay d855726 | `docs/streaming.md`, The clock |
| AirPlay 2: pairing's M3 0.52 s (was 1.0); next / previous by the sender's supported commands; a vanished sender's connections closed after 30 s | 2026-10-30 | `docs/streaming.md` |
| The kernel's idle loop on WFI (the boot stub's patch): playback, Warp0's AAC, the network and the sync as before; `top` 77 % idle during an AAC stream | 2026-10-30, `os.bcd` 515c689 | `docs/backlog.md`, Idle power |
| AirPlay 2 remote control and volume both ways (MediaRemote commands to a Mac's Music; senders told the speaker's level); a second sender takes over and the first is disconnected; the speaker presents itself as a Zeppelin Air by Bowers & Wilkins; the accessory identity kept in `/data` | 2026-10-28 | `docs/streaming.md` |
| AirPlay 2 on the reworked receivers and the restructured shairplay (`module/os/forks.md`): the iPhone (a track, a seek, pause and play, the next track), the Mac's system audio, the Apple TV's lossless; an iPhone as the second sender starting at the speaker's volume, the Mac's Music and the iPhone taking the speaker from each other, a disconnect in Control Center; a PIN asked and paired, and after it is removed no sender asks for it | 2026-11-02 | `docs/streaming.md` |
| zd1-airplay against what shairplay's review found (an AirPlay 1 DMAP length near 2^32, a plaintext SETUP or ANNOUNCE with a PIN set; the rest in the fork's tests): the DMAP probe answered 200 with the receiver running on, an ANNOUNCE before pairing refused 470 | 2026-11-02, system ebe57e6 on | `module/os/forks.md` |
| AirPlay 1 from a test sender, decrypted and decoded on Warp0, sample for sample the source | 2026-10-24 on | `docs/streaming.md` |
| AirPlay 1 asks for lost packets again (10 missing, 20 asks in 10 s over Wi-Fi; the test sender doesn't resend, so recovery is unseen); zd1-airplay lives on at a full `/tmp` with its log there | 2026-10-31, system 5f66c37 (by hand before the install) | `docs/streaming.md`, Lost packets |
| **Snapcast**: a snapserver found over mDNS, its stream played (PCM and FLAC, FLAC on Warp0), the server's client volume setting the speaker's; a network stall stretched, nothing lost | 2026-10-29 | `docs/streaming.md`; `module/os/README.md`, Snapcast |
| **Spotify Connect** (`zd1-spotify`): advertised, picked in the app, logged in, the queue loaded; Spotify refuses the test account every audio key (Not done, below), and the receiver stops on the track instead of skipping. The decode-and-output path (our Vorbis decoder on Warp0) plays a file sample for sample | 2026-10-25, 2026-10-28 | `docs/streaming.md` |

### Warp coprocessors and decoders
| What | Seen | Detail |
|---|---|---|
| Warp0 and Warp1 run our code from SDRAM and local RAM (one cycle per instruction there), under KnOS and under our OS | 2026-10-21 on | `docs/module/warp.md` sections 3, 6 |
| The Warp0 runtime: four images on one job loop (crypto; AAC, Vorbis and FLAC decoders), every pointer checked, a hang diagnosed and logged; the ciphers against the RFC / FIPS / SP 800-38A vectors and ~15,000 random jobs, the decoders bit-identical to their ARM builds | 2026-10-24/25/29 | `module/warp0/README.md` |
| Deliberate bus hangs on either core, parked afterwards, upset nothing measurable (18 hangs) | 2026-10-25 | `docs/module/warp.md` section 7 |
| Sol's ISA decoded for everything the vendor programs use and nearly everything else; SuluDMA block moves started by either Warp | 2026-10-22/23 | `docs/module/warp.md` sections 5, 9 |
| Our fixed-point decoders on the ARM926: AAC-LC 14.8 %, Vorbis 20.4 % (320 kb/s), FLAC (the Snapcast client 15.2 %) | 2026-10-24 on | `module/codecs/*/README.md` |

## Emulator only
None of these has been seen on the unit. Treat them as "the code does something plausible".
- **The debug services' default** in `os-debug.bcd` (on), and the MCU's choice kept across module reboots (the
  switch itself works on the unit).
- **A slot trial that fails on the unit** (the fall-back to the good slot after a panic or 90 s without ready), and
  `zd1 update` through the MCU from the rescue.
- **Parts of the MCU's behaviour** (`mcu/README.md`, Behaviour): the standby supply hold, two receivers with
  senders connected (`zd1 play spotify` pausing AirPlay).
- **Not seen anywhere** (they need USB audio, which the emulator doesn't carry, or a hot unit): a USB stream taking
  the speaker after 3 s of sound and waking it from standby, volume keys reaching the computer as HID keys while on
  USB, the ramp at a stream's start, the heat cap acting.
- **Snapcast**: the handover between Snapcast and AirPlay, the speaker's keys and knob reaching the server.
- **The relay with our current MCU firmware** (the relay against our OS is on the
  unit, above).
- **The IR decoder** (`mcu/src/ir.c`; its table matches the remote's Pronto codes; the remote was never tried).
- **Recovery tiers** not listed above (`docs/recovery/factory-reset.md`), the upgrade path stock -> ours -> stock.
- B&W's recovery-utility MCU image (erases the user page, sends HCR 0x2421, holds PB01 high); BL2's handling of HCR
  0x2421 and of the strap.

## Not done
- **Spotify Connect from a Spotify app**: Spotify refuses the test account the audio key of every track
  ("error audio key 0 1"), also to upstream librespot on the Mac with every login and client ID
  (librespot-org/librespot#1649); the other key path is Spotify's DRM. Not fixable in our code; needs an account
  that gets the keys. Spotify plays through AirPlay. Its credentials are not
  kept across boots.
- AUX optical (needs a mini-TOSLINK cable); IR with the real remote; the iPod dock (not implemented).
- Wi-Fi setup without a computer (the stock JBConnect flow).
- Our OS with the stock MCU firmware: not supported (it answers none of the stock MCU's checks: "9112", the
  network-settings backup; `docs/protocol/spi-registers.md`, section 4.2).
- Recovery that needs a programmer (BL1, a total brick); rewriting BL2 from a running image.

Work items: `backlog.md`. Checks that need a person at the unit: `units/<unit>/hw-checks.md` (not in git).

## Known problems on the unit
- **Snapcast's `playing` didn't take the speaker from USB once** [2026-10-29, system 1d9e63d]: after `zd1 update`
  restarted the module mid-stream (the speaker fell back to USB, asleep), zd1-snapcast reported `playing`, stopped,
  `playing` again, and the speaker stayed on USB until the mode key. The likely cause, in the MCU (5dddc50): the
  receivers' play state from before the restart is kept (`mcu/src/wifi.c`, `play`) and a receiver takes the speaker
  only when it changes, so a reconnected Snapcast's first `playing` counts as no change. Reset when the module
  greets again in MCU 515c689 (flashed 2026-10-30): not seen yet.
- **Telnet from the Mac times out after a module reboot** until the module has sent the Mac a packet (a ping from the
  module console fixes it); not every time. Not understood; the MAC's address filter is right.
- `sulu_eth`'s `TX packets` counter climbs at ~400/s with nothing to send (RX and IRQ counts are sane): cosmetic.
- The boot stub's own lines arrive garbled on the module console (BL1 leaves UART1 at another divisor); Linux's
  console is clean.
- The first boot after a `.bcd` install fills the partitions and takes ~46-61 s to `[os] up` (a normal boot ~16-22 s);
  the MCU waits for it (180 s).
- The Nokia kernel's `sulu_nand` "corrects" a good bit in about half of all 256-byte halves of the vendor's pages on a
  normal MTD read (`docs/module/boot-chain.md`, NAND): those are read raw, and never written through MTD.
