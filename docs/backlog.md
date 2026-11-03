# Backlog

Open work, grouped by area, roughly in priority order within each group. Each entry says what is wrong or missing
and what is known. What already works: `status.md`; checks that need a person at the unit:
`units/<unit>/hw-checks.md` (not in git). Remove an entry when it is done (the topic doc gets the result).

## Module OS
- **The page and the protocol's last pieces, checked on the unit** (`docs/web.md`, Order of work): on the unit since
  2026-11-01 (4f2215a), the Wi-Fi's status and the scan seen working; the page in a browser, the actions, the
  receivers' options, the amp's idle time and the Wi-Fi's failures are the open checks on the unit.
- **A small API on the module for home automation** (Home Assistant): the State record as JSON (the same schema,
  `docs/module/settings.md`), settings writes, play / pause / volume / source, with a credential of its own; the
  USB-only rule for settings relaxes then (`decisions.md`).
- **Setting up Wi-Fi without a computer**: the stock app's setup flow (JBConnect: the MCU's key combination, a setup
  network, the B&W app) is not supported by our OS; `zd1 wifi` over USB is the way. Smaller: a
  supplicant without the internal TLS library (PSK needs only SHA-1 / AES).
- **Power and heat**: the kernel's board init switches most clock gates off and `zd1-board up` turns the stock set
  back on (`dm870::clocks::Gates::STOCK_APP`); gates nothing here needs (LCD 17, Warp0 2 while idle, USB host 8, the
  AV ports' 3 until audio starts) could stay off (`docs/module/memory-map.md`, Clocks).
- **Idle power, what it saves**: the boot stub patches the kernel's idle loop to WFI (`linuxboot.rs`, `idle_wfi`;
  on the unit since `os.bcd` 515c689, 2026-10-30: playback, Warp0 and the network as before). Not measured: the
  module's power and temperature with it (the MCU's ADC4 / ADC5 against the days before).
- **Telnet after a module reboot** times out from the Mac until the module has sent the Mac a packet. Find out why
  (ARP?), or send a gratuitous ARP / one ping at boot.
- **The boot stub's UART1 output** is garbled after BL1 (another divisor): set UART1 up in the stub.
- **GPIO-17 "BIST activate"** (J2 pin 48, low-active, pulled up): what BL1/BL2 do when it is low at boot is not known
  (`docs/hardware/cr870-module.md`). Leave it alone.

## Streaming
- **AirPlay: groups with Apple's receivers** (`docs/streaming.md`, Synchronised playback): with an Apple TV and a
  MacBook in step by ear; open: realtime streams against the Apple TV (the 42.5 ms after the Mac's reported
  latency), a HomePod; AirPlay 1 on its NTP timeline; the output's ±0.5 ms steering band (ALSA's period-granular
  delay is the noise; a fractional trim of the AV PLL would follow the master without dropped frames).
- **Rates above 48 kHz**: on the module source the DSP's core runs at the CS2000's 44.1 kHz and the module's input
  goes through an ADAU1445 ASRC (a 48 kHz input plays at the right pitch with the CS2000 untouched [INF]), so a higher
  rate is converted down there; AirPlay never sends one. Snapcast turns anything but 44.1 kHz into 44.1 by linear
  interpolation on the ARM (it aliases, more so from 88.2/96 kHz); handing ALSA the stream's rate (snd-sulu takes
  32/44.1/48/96) would leave it to the ASRC, and 88.2 kHz, not in snd-sulu's list, would need a 2:1 decimator. 96 kHz
  through snd-sulu hasn't been tried.
- **AirPlay remote control, what is left**: the next key as a skip with a real podcast app (Podcasts on an iPhone:
  `the sender takes a skip instead` in `airplay.log`); DACP's `play` / `pause` with a real AirPlay 1 sender.
- **Pairing faster, what is left**: transient pair-setup's M3 takes 0.52 s (`shairplay` `crypto/mont.rs`, ~1.1 ms per
  3072-bit Montgomery multiplication, ~474 of them); a squaring of its own (252 of them are squarings) would save
  ~10 %. Pairing without a PIN stays transient, as for AirPlay speakers without HomeKit.
- **The receive path's cost**: with Warp0 decoding, a realtime stream still takes ~17 % of the ARM (null output):
  the kernel's UDP receive on Ethernet 5-9 % for 126 packets/s (`sulu_eth` block-moves every frame between SRAM rings
  and SDRAM), the receiver ~9 % (tokio, RTP, timing). Measure on Wi-Fi; profile the receiver.
- **The buffered stream's decryption** (ChaCha20-Poly1305 over TCP, ~4 % of the ARM) could go to Warp0 too, in the
  AAC image or as a job before the decode.
- **Spotify Connect** (`docs/streaming.md`): blocked by the audio keys Spotify refuses the test account
  (librespot-org/librespot#1649). With an account that gets them, or a way upstream: a whole session (the CDN
  download through the 1.5 MB window, AES-CTR, track changes, seek, volume, keys) and its CPU with the network side
  (estimated ~15-20 % with Warp0). Then: the credentials across boots (librespot's cache in `/data`); MP3-only files
  (some podcasts: nothing decodes them, they are skipped); AES-CTR on Warp0 (~2 % of the ARM); which TLS cipher the
  CDN picks (ChaCha20 would be cheaper than AES-GCM here); upstream's #1795 (a pick of a track in a list played
  another: `Failed to resolve index by Some(Uri(""))`, seen on the unit 2026-10-28).
- **AAC decoder against float references** when `module/codecs/aac` changes again: its output against Apple's
  AudioToolbox (`afconvert`) and FFmpeg's float `aac` decoder before rounding, so what's left is our error beyond
  16-bit rounding.
- **Upstream** the shairplay-rust, librespot and libmdns changes (`module/os/forks.md`; the librespot playback one is
  librespot#1799 already).
- **Sol owning the audio output** (an option, `docs/streaming.md`, Where the Warps fit): PCM written once, SuluDMA to
  AV port 2, resampling/DSP on Sol. Needs the port and the PTU FIFO driven without snd-sulu; saves little (the ALSA
  path is ~1.8 %).
- **Warp0 runtime** (`module/warp0/`): the cause of the first Warp0 bus hang (2026-10-24, before the pointer checks
  and the diagnosis existed: if one comes back, its log line and replay file are the start); hangs during a codec job
  with SuluDMA or Sol active, and on other addresses (not tried); ALAC faster (~300 k cycles a packet: 32-bit
  predictor sums, a word-wise bit reader); a Vorbis profile; ChaCha20 by hand.

## MCU firmware
- **Calibrate the heat limit.** The soft limit (ADC5 < 0x280 or ADC4 < 0x248 for 3 s, ~66 °C, caps the volume at
  step 50) is a guess between the hottest seen (~51 °C on ADC5) and stock's trips (~81 °C;
  `hardware/zd1-main-board.md`, B assumed). The firmware logs both every 10 min of amp-on: collect a few long
  sessions, then set the limit (and decide whether a hard limit above stock's trip is worth having).
- **Pausing the computer when another source takes over** (HID play/pause when AirPlay or AUX takes the speaker from
  a playing USB stream): not done. On macOS a play/pause key with no app "now playing" starts Music.app, and the sound
  may come from an app the key doesn't reach; today the USB stream keeps running unheard and the speaker goes back to
  it once the other source is silent.
- **Stable USB identity**: macOS asks "Allow accessory to connect?" when the descriptors change; keep them as they are
  between builds (the serial number is fixed, "ZD1").
- **The volume cap** (step 40 of 71, `mcu/src/usbaudio.c`, for USB and the module) stays until it is removed on
  purpose.
- **The host link**: events as typed messages (source and module changes, keys) instead of only log text; a WinUSB
  descriptor for Windows.
- **The transport keys after the CDC port went** (MCU feffd90; HID keeps play/pause, next, previous): not tried on
  the unit (a key starts Music on whatever the Mac plays to): the speaker's play/pause and next reaching the Mac.
- **iPod dock** (USB host + iAP, `docs/mcu/stock-firmware.md`): not implemented.

## Updates and recovery
- **1 Mbaud loses ~9 % of the update's frames** (871 of 9922 sent again; 460800: 7), a third of the line's speed
  (61 KB/s of ~95). Where the bytes go is unknown (the guess: the module's UART FIFO overruns while the NAND is
  written). Worth it only if updates must get faster: the driver's overrun bit, JTAG, or a larger frame window if the
  losses come in bursts (`module/updater.md`).
- **The web page as the installer** (`docs/web.md`: the plan, in its order of work): `zd1 install` with the relay and
  `zd1 rollback` on the unit as one flow (`install --no-bcd` has run there); the Pages workflow's first run on GitHub;
  the page's three views (triage, installer, updater and configurator) with backups kept in the browser; interlocks
  (`docs/web.md`, Interlocks).
- **Rewriting BL2** from a running image: the stock app's `fburn ebin 0 0xA0000 0x160000` + XModem over the module
  console (`zd1 console`; its y/n confirm never sees an answer over telnet). Untested.
- **The MAC back without the stock app**: BL2's serial menu "8 program MAC0" on the module console. Untested.
- **A NAND programmer image**: the spare-area layout and ECC are known (`module/boot-chain.md`); needs the chip's
  bad-block list and whatever the boot ROM expects of block 0.
- **MCU JTAG pads** are not located (needed only for a total brick).
- **The UART path at 2 Mbaud**: the MCU's USART1 by DMA (PDCA) both ways and the host's side in blocks, not bytes (at 2
  Mbaud the module side is clean, ours drops: `docs/hardware/cr870-module.md`, UART speed).

## Emulator
Places where the emulator is known to differ from the unit; fix the model when one gets in the way.
- **`sulu_eth` "tx DMA queue full" panic** under streaming load (about 1 run in 4 with several sessions in
  parallel), and reliably after a `MEMREADOOB` ioctl at boot (bisected: the ioctl alone; the NAND model has no IRQ).
  The panic is sulu_eth's count of pending block moves (priv +0x1478) reaching 7. Not understood. The unit did 65536
  OOB reads while streaming the NAND backup without it.
- **IRQ timing with both boards linked**: KnOS's 8 kHz tick ISR (0x401280) advances its packet timestamp twice for
  ~13 % of AirPlay packets (a 440 Hz tone played as ~514 Hz); ~40 % of the ticks arrive while the previous one is
  pending or in service. Suspects: IRQs masked in a delay-loop halt, the 200 µs linked quantum.
- **Sol model** (`CR870Sol.cs`) is KnOS's reference model, not the hardware (`module/warp.md` section 8). Fix before
  running a vendor Sol program in the emulator.
- No WILD core (its SRAM is modelled); NAND without bad blocks or bit errors; USB carries EP0 only (no bulk pipe,
  audio, HID or status interrupt).
- AV port 2 ctrl `On` stays 0 in emulation (live: 1); nothing seems to need it.
- The stock MCU in emulation gets the module's `play` status but doesn't switch to Wi-Fi (on the unit it does).
- Bridged networking (or mDNS out of the emulator), so a real Mac / iPhone / snapserver can reach the emulated
  module; listening to the emulated DSP output live.

## Tooling and project
- **Stereo, level-calibrated probe**: `usprobe` uses the MacBook's single mic and can't run while other audio plays.
- **`musictest.py sync`'s network-timeline figure** is off by the same ~0.5 s for both devices since senders may stream
  4 MB ahead (its reference assumes the first frame sent is the track's start).
- Ideas: CI (build everything); `zemu run`'s
  steps against the real speaker (host link + usprobe + module ping, with an amp-time budget); a
  "porting to your CR870 device" page; tagged releases of the MCU `.hex` and the module image.
