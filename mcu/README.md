# Our MCU firmware

Replacement firmware for the Zeppelin Air main board (AT32UC3A1256), built on Atmel's ASF. It does what the stock
firmware does for USB, AUX and AirPlay through the stock module, plus a host link for tools (typed requests over USB,
the network module's console among them). It links at 0x80004000, so the B&W DFU bootloader stays in
place and any build can be undone (`docs/mcu/bootloader-dfu.md`).

## Status (on the unit)

| | |
|---|---|
| cold boot, self-DFU (`Dfu`), the build's identification, `mcu/flash.sh` | works |
| the host link (typed requests over a vendor interface, the log; no text console), the gate's press and `gate open`, the bulk pipe, the settings relay, the debug switch, the gate locked and the interface claim | works (2026-10-26, 2026-11-02) |
| audio bring-up (CS2000, DSP download, codec), amp sequence, protections | works; the amp allowed from every boot (the default; `zd1 amp deny` locks it, at boot too) |
| sources: USB, AUX analog, the module running our OS; ON/OFF cycling, standby, volume keys, settings | works |
| USB audio (UAC1 48/44.1/32 kHz, adaptive with CS2000 trim) + HID media keys | works (0 under/overruns) |
| the behaviour on top of stock's (below, Behaviour): the amp asleep on a silent boot, woken by sound, off after 5 min of silence; picks (AirPlay, the computer's output, AUX); settling; the LED's looks; stock's USB strings | works (2026-10-26/27; the LED's looks, the idle time and the volume cap set by `prefs`, the computer's volume over the whole cap, no false pick when the Mac wakes: 2026-11-02). Not seen yet: a USB stream taking the speaker after 3 s, the ramp, the heat cap acting (no USB audio in the emulator); the standby supply hold: emulator only |
| module link: our OS's native registers (`docs/protocol/sequences.md`; the stock app is detected and reported, not driven); the system slot the module boots; `zd1 update` to the module; HCR 0x21 into BL2, BL2's XModem (`zd1 xmodem`); the stock app's shell on telnet | works |
| IR remote | decoder runs in the emulator; never tried with the remote |
| AUX optical | untested (needs a mini-TOSLINK cable) |
| iPod dock | not implemented |

Open work: `docs/backlog.md`. Checks that need a person: `units/<unit>/hw-checks.md` (not in git).

## Build, flash, talk to it
```
nix build .#mcu -o mcu/result        # -> mcu/result/zd1.{elf,hex,bin,map}, ~15 s when changed
mcu/flash.sh                         # that, then DFU (the host link's Dfu, or already in DFU) -> a DFU backup into
                                     #   units/<unit>/mcu/dfu-backups/ -> flash + verify + start
                                     #   -> the new firmware's version (must be the image's) and boot-stage log
zd1 info      # versions, the speaker's state, the module
zd1 diag      # internals: protections (ADC5 = temperature), audio path, USB audio, AirPlay, IR
zd1 log -f    # what the firmware prints, the module's console (its UART) among it
zd1 console   # the module's console (a terminal; `zd1 console run CMD` for shell commands to their end)
```
`zd1 --help` has the rest (source, volume and keys, DFU, reboot, the amp interlock, the module's power and
registers, peek/poke, the module OS's debug services); `zd1 settings` the module OS's settings (`docs/module/settings.md`).
- `Makefile` builds it with Microchip's AVR32 GNU toolchain; nix runs that (`nix build .#mcu`, `nix/mcu/`: the
  toolchain is an x86_64 Linux binary, so on a Mac a Linux builder runs it; nix fetches it from archive.org,
  `CONTRIBUTING.md`, Builds). Nix builds what git tracks: `git add` a new source file first. ASF 3.x and the device headers
  (Microchip UC3A DFP) are vendored in `asf/` and `dfp-include/`. `asf/` holds only what the build reaches (the
  Makefile's sources and the headers they include, in the unit's and the emulator's builds); a driver added to the
  Makefile comes from ASF 3.x with its headers. Two local patches, each marked `zd1`/`ZD1` in the file:
  - `asf/avr32/drivers/twi/twi.c`: every wait for a transfer is bounded (20 ms + 1 ms per 16 bytes), then the
    transfer is aborted; ASF's waits forever, and a stuck bus (the DSP unpowered) hangs the MCU.
  - `asf/avr32/drivers/usbb/usbb_device.c` (`udd_ep_abort`): on an OUT endpoint the busy banks are handed back;
    ASF's KILLBK loop only works for IN banks and never ends on an OUT endpoint holding a packet (the pipe's abort).
- The firmware identifies itself: `src/version.c` holds a block the nix build stamps with the commit (`-dirty` with
  uncommitted changes) and its date after linking (`mcu/stamp.py`; on the host, so a new commit doesn't need the Linux
  builder). `zd1 image FILE` reads it from an image or a DFU backup; the firmware reports it in `HelloReply`,
  `zd1 info`, and tells our module OS (SPI reg 0xC3).
- The emulator runs `nix build .#mcu-emu` (`ZD1_EMU`: the log mirrored on USART0, the run's `mcu_log.txt`); the host
  link works there through the emulated USB (`zemu run --mcu --host 'zd1 info'`, or `--keep`
  and `ZD1DFU_EMU=127.0.0.1:4620`), with the gate opened in the run's copy of the EEPROM. `zd1 dfu boot-log` reads the cold-boot stage log over DFU.
- Only flash with the unit owner's OK, with a DFU backup first. Recovery from anything: hold ON/OFF at power-up → DFU →
  `zd1 dfu`.

## The host link
Typed requests and the firmware's log over USB, defined in `protocol/host/src/lib.rs` (reference:
`docs/protocol/host.md`; the C here, `src/zd1host_gen.h`, is generated from it). Transport:
control transfers on EP0 to a vendor interface (IF3, class FF/5A/01: the host tools find it by its class); USB checks and retries every transfer, a message is one
transfer. Bulk data (an update's image) goes to the interface's one endpoint, bulk OUT 0x05 (the pipe, `src/pipe.c`).
The USB interrupt takes a request and hands out the log (`src/log.c`) and the reply (`src/hostif.c`); the main loop
runs the request. It is the only way in: no text console, every command a typed request with a typed reply. Requests:
`Hello` (also answered from the interrupt), `Info`, `Diag` (internals), `Select`/`Volume`/`Key` (the source, volume and
bass, a key as if pressed), `Amp` (the interlock), `Peek`/`Poke` (bytes or 32-bit words), `Dfu`, `Reboot`, `Module`
(power, bring-up, into BL2 over SPI or through BL1's serial menu, held powered for JTAG), `ModRead`/`ModWrite`, `GetSetting`/`SetSetting` (the module OS's
settings), `Debug`, `Unlock`/`Lock` (the gate), `Prefs`, `BootState`/`Boot` (the module's
system slot), `Update`/`UpdateStatus`/`Pipe`/`PipeData` (the module's update, the bulk pipe), `XmFrame` (BL2's
XModem), `Console` (the module's console for this host: its bytes in POLL's CONSOLE records instead of lines in the
log; closed when the host stops polling for 2 s or a new host says HELLO) and `ConsoleWrite` (the host's bytes into the
1 KB send ring, as many as fit; `zd1 console`, `src/modcon.c`). Everything the firmware prints goes into a 4 KB log the host reads (oldest text
dropped when nobody reads; a reading host gets up to 50 ms to make room). The protocol only grows (old and new
firmware and tools meet: `docs/protocol/host.md`, Compatibility); `HelloReply` lists the requests the firmware has.

**The gate.** Anything on the computer can reach the USB port, a web page the user lets at it included, so the
requests that change the speaker (flashing, settings, the debug services, memory, the module's power, the source
and volume) need a press of ON/OFF on the speaker: `Unlock` waits 30 s for it (the LED flashes purple, the press does
nothing else), then they run until 10 minutes after the last one. A new firmware's first start (its version isn't the
one the EEPROM's second page recorded: it was just flashed) is unlocked the same way, so an install goes on without a
press: whoever could flash it could change anything already (`hostif_first_start`). `zd1` asks for the press by itself. Reads
(`Hello`, `Info`, `Diag`, `GetSetting`, `ModRead`) are free. For a unit tools drive unattended, the gate can be opened
for good (`zd1 gate open`, itself behind a press; kept in the EEPROM flags byte; `gate lock` closes it).
Emulator runs with our firmware have it open (zemu sets the flag in the run's EEPROM). The host tools claim the vendor
interface: one program at a time (another tool, a browser tab) gets refused.

The network module's console goes over the host link (`src/modcon.c`): 115200 on the module's UART; its bytes come
raw in POLL while a host has it open (`Console`; what the module printed before is in the log, a line at a time), the
host's go by `ConsoleWrite`. The MCU itself talks on it for BL1's escape only (`Module ESC`: 0x02 from the reset's
release until BL1's menu, then `4` CR) and while the module's updater has it.

## Commands (`zd1`)
The host link's side of `zd1 --help`, each a typed request or a few; `zd1 COMMAND --help` has the options.

| | |
|---|---|
| `info`, `diag`, `hello`, `log [-f]` | the state at a glance; internals (protections, audio path, USB audio, the module's link, the last IR frame); the version (answered from the interrupt); the log |
| `play usb\|aux\|wifi\|ipod\|airplay\|spotify\|snapcast\|standby`, `standby`, `volume [STEP] [--bass N]`, `key onoff\|hold\|standby\|volup\|voldn\|play\|next\|prev` | the source manager, as the keys and the remote drive it (USB and AUX only while plugged in, a receiver with a sender connected; `ipod` refused: no dock support) |
| `amp allow\|auto\|deny` | the amp interlock: allowed until the MCU restarts / allowed from every boot (the default: the amp comes on by itself when there is sound) / locked, at boot too (EEPROM flag `SETTINGS_AMP_LOCKED`, until `amp auto`) |
| `prefs [--auto-switch on\|off] [--amp-idle MIN] [--volume-cap N]` | a USB stream taking the speaker by itself; how long a silent source keeps the amp on (1 to 30 min); the highest volume on every source (1 to 71, 40 unless set); all in the EEPROM |
| `gate [unlock\|lock\|open]` | the host link's gate |
| `module off\|start\|power\|restart\|bl2\|hold\|esc [-t S]` | the network module: `start` = the non-blocking bring-up done at every boot (off again after 180 s without READY; a system slot not ready in 90 s is power-cycled onto the next choice); `power` waits ≤ S (120); `bl2` HCR 0x21 into BL2; `hold` power-cycled, the reset released, nothing watching (JTAG); `esc` into BL2 through BL1's serial menu, whatever the image (USART1: PA06 → the module's RXD1, PA05 ← TXD1) |
| `console [run CMD...\|log]` | the module's console: a terminal (Ctrl-] leaves), shell commands to their end (output and exit status), or what it says |
| `state`, `settings`, `name`, `wifi`, `ip`, `receiver`, `pairings`, `servers`, `reset` | the module OS's State record and settings (kept by the MCU from the module's pushes, SPI reg 0xD6; `docs/module/settings.md`) |
| `update IMAGE`, `slot [try a\|b\|rescue \| good a\|b \| clear] [--restart]` | the module's system into the other slot over the bulk pipe (`docs/module/updater.md`); which system slot the module's boot stub starts (`docs/module/storage.md`, The boot) |
| `xmodem IMAGE.bcd [--in-bl2]` | a module image through BL2's XModem over SPI (`Module BL2` first); BL2 erases the app partition first |
| `reg read\|write F [--v]` | module registers over SPI (the stock app's shell on TCP: three register writes, `docs/module/knos.md`) |
| `debug on\|off\|default` | our module OS's debug services (RAM only: told to the module at every ready) |
| `peek`, `poke` (`-w`: 32-bit) | memory and registers (GPIO pin = port·32 + bit); `poke` refuses the flash controller and the bus matrix |
| `bootloader`, `reboot` | into DFU (watchdog, ~3 s) / restart the firmware |
| `bulk-test [MB]` | the bulk pipe into DISCARD: throughput and CRC-32 (930 KB/s; ~810 KB/s while USB audio streams) |

The rest drives the DFU bootloader (`zd1 dfu ...`, `docs/mcu/bootloader-dfu.md`) or both: `flash`, `install`,
`rollback`, `image`.

## Source layout

| Path | What |
|---|---|
| `src/main.c` | boot order (pins on the RC clock, then OSC0, then PLL0 = what works on a cold boot) and the main loop |
| `src/zd1_board.*` | pin map, safe GPIO, clocks (48 MHz from one PLL for CPU and USB), stage log in flash page 511 |
| `src/hostif.*`, `src/pipe.*`, `src/udi_hostif.h`, `src/zd1host_gen.h`, `src/version.c` | the host link (requests and replies), its bulk pipe, its USB interface, the protocol (generated), the firmware's identification (stamped by the build) |
| `src/log.*`, `src/util.h`, `src/selfdfu.*` | the log (a ring the host link reads), the bounded string copy, DFU entry and reboot, the watchdog |
| `src/i2c.*`, `src/dsp.*`, `src/dsp_data.*`, `src/codec.*`, `src/cs2000.*`, `src/amp.*`, `src/audio.*` | audio chain; `dsp_data.c` is generated from the stock download (`tools/gen_dsp_data.py`) |
| `src/source.*`, `src/inputs.*`, `src/led.*`, `src/ir.*`, `src/protect.*`, `src/settings.*`, `src/tick.*` | behaviour: sources, keys, LED, IR, protections, EEPROM settings at 0x80 (two pages, each with its magic and CRC: 0x80 volume, bass, source, flags; 0x88 the amp's idle time and the volume cap) |
| `src/udi_audio.*`, `src/udi_hid_cc.*`, `src/usbaudio.*` | USB audio class (ASF has none), HID consumer control, the streaming engine |
| `src/dm870.*`, `src/modboot.*`, `src/update.*`, `src/wifi.*`, `src/modcon.*` | module link (stock protocol and timing; our OS's version, settings, debug mode), which system slot the module's boot stub starts (the good one and a try in the EEPROM, the fall-back when a boot doesn't get ready: `docs/module/storage.md`), the module's update (the bulk pipe to its console UART as it grants it: `docs/module/updater.md`), the AirPlay glue, the module console on USART1 (its bytes to the host that opened it, `Console`; lines into the log otherwise) |
| `src/zd1link_gen.h` | generated from `protocol/spi` (registers, kinds, DRC codes, the spec's values): don't edit |
| `src/zd1link.h` | the SPI protocol's code on those values (stuffing, the volume conversion, net status, the play-icon matcher), held to the Rust by `test/zd1link_gen_test.c` |
| `config/` | ASF `conf_*.h` (`conf_usb.h` = the composite layout) |

## Behaviour (ours, on top of stock's)
All in `src/source.c` unless said otherwise; the times are constants there.
- **The amp follows the sound.** A source silent for 5 minutes (the default; `zd1 prefs --amp-idle MIN`, 1 to
  30, kept in the EEPROM's second page: the amp runs hot, so never longer) has its amp switched off (the
  LED dims); sound brings it back with the normal sequence (supply, 2 s soft start, unmute, ramp), so the first ~2.5 s
  after a long silence are lost. "Sound": USB, the host sends samples over -66 dBFS (`usbaudio.c`, looked at whichever
  the source); Wi-Fi, our OS's receiver in front plays; AUX (analog and optical), the DSP's input meter (param 0x031, read every 50 ms; it measures the selected input
  only, before the volume stage): sound is 12 dB over the source's own noise floor, learnt (down at once, up ~1 dB per
  24 s, again from scratch when AUX is plugged in), the threshold within -80..-40 dBFS (a MacBook on AUX: floor -86,
  music -66..-28 [HW]). A volume, play or track key wakes a sleeping amp too (a source too
  quiet for the meter). A power-up with nothing to play leaves the amp off until there is sound. All of this while
  the interlock allows the amp: from every boot unless `amp deny` locked it (EEPROM).
  Standby holds the supply on (muted) for 30 s (`amp_release`), so a quick wake doesn't cycle it. The computer opening
  its stream while USB is the source and the amp sleeps starts the amp at once (sound is likely coming: a track loses
  less of its start; a beep brings it up for one idle period).
- **Settling** (`settle()`): a source silent long enough moves the speaker to where something plays or can be heard:
  the computer if it plays (after 5 s from a source that took over by itself, so AirPlay can interrupt the computer;
  else 15 s); else AUX if plugged in (the meter hears AUX only while it is the source); else, if the source isn't
  there any more (its sender left), the computer if connected. 60 s instead of 15 with a receiver paused in front (a
  phone call); never within 30 s of a choice made by hand. A source entered by itself while silent gets the amp
  asleep (the supply held) and, on Wi-Fi, no DF: a paused phone isn't resumed by it.
- **When a source goes away** (its plug out): what plays, else the last source if still there, else AUX, a receiver
  with a sender, the computer; standby only when nothing is. Waking (a key, power-up) the same, else the last source.
  Play on the remote in standby wakes it and plays (Wi-Fi: the receiver in front is chosen, 0xC2, and resumes; USB:
  the HID key). The module gets the speaker's snapshot at its hello, nothing is resumed by a boot.
- **Standby is "do not disturb"**: off until a person asks for sound. It mutes, pauses the module's receiver and
  lets the amp go (the module, the DSP and USB stay up, so it can be reached; power-wise it is the same as a source
  with its amp asleep). Only what a person does wakes it: ON/OFF, the remote's standby or play, a plug going in, a
  pick (the speaker chosen on a phone, in Spotify, as the computer's output), a phone or app starting to play to it,
  the computer playing 3 s without a break. Background activity doesn't: a beep, settling, sound on AUX (a TV
  turning on). Into it: ON/OFF held 2 s, the remote's standby, `zd1 standby`, the last plug out with
  nothing else connected, ON/OFF after a protection trip.
- **Sources as a person sees them**: the computer (USB), AUX, and each of the network module's receivers (AirPlay,
  Spotify, Snapcast), not the board's inputs. One is available while it is connected, playing or not: a cable or a
  plug in, a sender connected to the receiver (our OS lists them on SPI reg 0xC6).
- **The ON/OFF key**: in standby a press wakes onto the last source (else USB, AUX). Playing, a short press steps to
  the next available source in stock's order (iPod, USB, AirPlay, Spotify, Snapcast, AUX); the LED pulses in its
  colour and the switch happens 1 s after the last press. Nothing else available: nothing happens. Choosing a
  receiver resumes its sender and pauses the others (SPI 0xC2, `zd1-linkd`); a sender that can't be resumed from here
  (AirPlay without a remote) stays paused until it plays. Held 2 s: standby. After a protection trip a press
  acknowledges it (standby); until then nothing brings a source back (a stream, a plug, the host link). While a tool
  asks for the gate it confirms that and does nothing else. The host link does the same: `zd1 play
  airplay|spotify|snapcast`.
- **Newest wins.** Whatever starts playing takes the speaker: a receiver through its play state (the module decides
  among its own, `module/os/README.md`; the speaker follows its `pl`), AUX by being plugged in, and a USB stream that
  starts, once it has sounded 3 s without a break (from standby too; a system sound doesn't: with 1 s, a 1.4 s
  alert took the speaker from paused AirPlay on the unit). When nothing else is connected (no AUX plug, no sender on
  our receivers) and the speaker runs, there is nothing to protect: the stream's
  opening switches to USB (its amp starts while the sound is on its way), and its first sound if that was missed.
  Standby keeps the 3 s (a notification shouldn't wake the speaker). Going back to a stream that kept playing:
  Settling, below. Nothing switches by itself within 8 s of a choice made by hand (keys,
  the remote, a plug, the host link's `Select`), and a stream that was playing before the current source began doesn't
  count as new (a gap between tracks shorter than 10 s doesn't end a stream). Leaving the module pauses its receiver
  (the snapshot says it isn't the input, its receivers pause a sender that plays). `prefs auto-switch off` turns the USB part off.
- **Intents**: an explicit pick gets an answer at once, before anything plays: plugging in AUX or a USB cable,
  picking the speaker for AirPlay on a phone or a Mac (the sender's first session SETUP, not its probes), picking it
  in a Spotify app. The speaker switches to it as a choice made by hand (8 s without automatic switching), the LED
  pulses twice in its colour, and the amp starts (its 2 s soft start runs while the sender gets going). A pick doesn't
  make the sender play (Wi-Fi is entered without DF). The module's picks arrive on SPI 0xC0 (`zd1-linkd`, `intent
  SOURCE`). Picking "Zeppelin Air" as the computer's output: macOS opens the stream at once, sends
  silence and closes it ~2 s later; switching away does nothing on USB; a system sound opens it with sound [HW
  2026-10-27, three picks]. A stream that opened and closed silent after 0.8-5 s is a pick (`usbaudio.c`), answered
  when it closes, ~2 s after the click. Every request the host makes of the audio interface is in the log (`[usb]
  host: ...`). Snapcast has no pick to see (the client connects by itself).
- **Volume keys on USB** step the speaker's volume, as on every source, and USB audio's status interrupt tells the host:
  its slider follows (stock sent HID Volume Up/Down to the host in PC mode instead). A USB stream's start or rate switch mutes and ramps back up in 200 ms (`source_ramp_in`).
- **USB identity**: stock's strings, "Bowers & Wilkins" / "Zeppelin Air", and a fixed serial number ("ZD1"), so the
  host shows the speaker under its name and keeps its settings; the VID/PID stay 03EB:2FE0 (the tools find it by them).

## The LED
`src/led.c`. The source manager describes what the speaker does (a look: standby, error, the source, the module's
receiver, playing / ready / asleep / no network / starting / choosing); `led.c` draws it. B&W's own patterns are not
used (`re/mcu/extracted/led_patterns.txt` has them).

| | |
|---|---|
| power-up | red breathing |
| standby / error | dim red / red blink |
| USB, AUX, iPod | sky blue, orange, magenta-pink |
| Wi-Fi: the module starting / no network | white, slow breathing / white pulse |
| Wi-Fi: nothing playing / another app, AirPlay, Spotify, Snapcast | white (Wi-Fi's states by animation), white, lime, green-aqua. Our OS names its receiver (SPI reg 0xC6) |
| playing / ready (silent, paused) / amp asleep | steady / breathing (4 s) for the first 30 s of silence, then dim (the amp itself sleeps after its idle time, 5 min unless set) / dim |
| ON/OFF choosing a source | the candidate's colour, a fast pulse |
| volume | the brightness shows the level for a second; red blinks at the ends |
| the gate | a fast purple blink while it waits for the press, green / red after |
| changes | 450 ms crossfades |

Colours and levels are in perceptual units (0..255, the PWM gets the square): even fades and breathing on a 400-step
PWM. The palette is fitted to this LED as a camera measured it (a duty sequence filmed in Apple Log, 2026-10-27:
red weak, blue strong, equal duty a bluish white), for the smallest CIEDE2000 between looks
that must be told apart (19.7; `mcu/tools/led_palette.py` with the measured model, `mcu/tools/led_model.json`). `zd1 diag` shows the look.

## Design notes
- **USB**: composite UAC1 (IF0 control: feature unit mute/volume, a status interrupt IN EP 0x82; IF1 streaming:
  PCM16 stereo, iso adaptive OUT EP 0x06) + HID consumer (IF2: play/pause, next, previous) + vendor (IF3: the host link
  on EP0, the module's console among it, the pipe on bulk OUT EP 0x05, two banks: no faster than one, ~910 KB/s, the
  MCU's handling of its 512-byte buffers is the limit), 03EB:2FE0, stock's strings ("Bowers &
  Wilkins", "Zeppelin Air") and serial "ZD1"; no serial port. The endpoint RAM is under 1 KB (`config/conf_usb.h`):
  audio 512, the pipe 128, HID and the status interrupt 8 each; EP3 and EP4 are free. The pipe sits below audio, whose
  alternate-setting switches re-allocate every endpoint above it. The status interrupt (`src/udi_audio.c`): a status
  word from the feature unit when the volume changes on the speaker (the keys, the remote, the page, the module's
  senders), so the host reads it back and its slider follows; the host's own changes send none. macOS honours it [HW
  2026-11-02, feffd90 and a test build before it: every change on the speaker sent one word, the Mac took it, read the mute and the volume
  back, and CoreAudio's volume for the device followed (steps 25, 26, 27 -> -17, -15, -13 dB), with the speaker not
  even the Mac's default output]. So the volume keys step the speaker's own volume on every source; HID carries only
  the transport keys. **No
  more than 16 bits at 44.1/48 kHz** fits a USB audio class stream here: the largest endpoint is 256 bytes (PEP5/6;
  datasheet 32058K, Tables 30-1/30-2; UFEATURES 0x00012467 on the unit: endpoint RAM under 1 KB) and an isochronous
  one needs two banks, so a 1 ms frame carries at most 256 bytes: 48 kHz/16-bit takes 196 (with the adaptive extra
  frame), 44.1 kHz/24-bit would take 270, 88.2 kHz/24-bit 534. Higher resolution would need our own host driver
  sending PCM through the bulk pipe (530 KB/s for 88.2/24, of its ~810) and gain nothing audible (the volume is in the
  DSP, so 16 bits carry the source at full scale); it goes over the network instead (`docs/streaming.md`, Formats). Host volume −60..+10 dB spans the steps 1..the volume cap, whole dB both ways (`src/hostvol.h`: the whole of the
  computer's slider does something, as the module's senders' 0..100 %); one
  limit holds on every source, whatever sets the volume (the keys, the remote, the host link, the computer, the
  module's senders): the volume cap (`zd1 prefs --volume-cap N`, 1 to 71, 40 unless set, kept in the EEPROM's second
  page; a lower cap takes the volume down at once, and the module maps its senders' 0..100 % onto 0..the cap). The
  heat cap (step 50 while hot, `protect.c`) holds it lower on top.
- **Audio engine**: iso packets → 1024-frame ring → PDCA ch0 blocks of 64 frames → SSC as stock (`ssc_i2s_init(.., 16,
  32, STEREO_OUT_EXT_CLK)`: the bit clock comes in on RX_CLK PA18 from the CS2000/MCLK chain). Prime to 512 frames,
  silence on underrun. Rate: a PI loop on the ring fill trims the CS2000 ratio (~0.93 ppm per LSB) once a second.
- **Silence pitfalls**: codec reg 4 bit 0 (DAC master mute) and PB27 (codec mute pin) both have to be
  cleared after the amp unmute, like stock. The DSP "soft mute" records are path selectors, not a mute.
- **The watchdog**: ~2.3 s, fed once per main-loop pass (`src/selfdfu.c`); a loop that stops resets the MCU, which
  takes the amp down, and the next boot logs `[boot] the watchdog reset the MCU`. Every wait in the loop stays well
  inside it: an SPI read has 300 ms, a V write's three tries ~1 s, `XmFrame` waits 500 ms at most (the host asks
  again), and what takes longer runs from the loop, answered when it ends: `Unlock`'s press, `Module POWER` / `BL2` /
  `HOLD` and every power cycle, the module OS's settings and records.
- **Rules learned**: never block the main loop (no waiting prints in USB/audio paths); one owner per timing decision
  (two timers for the unmute raced); ASF drivers, not hand-rolled registers (a home-made `delay_ms` once read the wrong
  system register and hung everything); the watchdog survives its own reset, so self-DFU uses PSEL 16, never ASF
  `wdt_reset_mcu`; don't override the bootloader's USB mux state.
- **Module timing**: stock attach (PA04, 1.2 s, PA17, 0.2 s), read 0x11 only while PA20 is high; `Module BL2` leaves
  SPI quiet while BL2 starts.

## Tests
On the unit: `mcu/flash.sh` (flash, then the version and the boot-stage log over the host link). In the emulator, for
development: `python3 emu/zemu run none --host 'zd1 info'` (the main board alone, the module
stand-in on J2; the host link through the emulated USB) or `zemu run --mcu --keep` (linked to our module OS; the
MCU's USB on 127.0.0.1:4620). Protocol conformance, offline: `cargo test -p zd1-hostlink` and
the C test (`protocol/README.md`). The emulated USB enumerates our firmware and carries EP0, nothing on the other
endpoints: audio, HID, the status interrupt and the pipe need the real device (the pipe's data goes by `PipeData`
there).
