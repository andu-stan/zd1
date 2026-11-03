# The module OS: our userspace on the CR870 module

Linux for the network module, built by us: Nokia's HD-1 kernel (2.6.24.7, `mach-sulu`, unmodified binary: it has the
drivers for this chip) with **our** root filesystem: static busybox, our init, our apps in Rust; from Nokia's
rootfs only the kernel modules and the Wi-Fi firmware (`os.toml [vendor]`). Everything is described in `os.toml`, built by nix
and put together by `assemble.py`; the emulator boots it with `zemu run` (image `os`, the default).

On the unit it boots from its system slot, joins Wi-Fi from the settings, and plays AirPlay, Snapcast and (where
Spotify gives the keys) Spotify Connect through the speaker (`docs/status.md`). Install (`docs/module/updater.md`):
the system with `zd1 update` (`emu/img/os/system.cpio.gz`) into the slot that doesn't run; the stub, the kernel
and the rescue (`os.bcd`, rarely) with the relay over USB (`zd1 bootloader`, `zd1 dfu relay
emu/img/os/os.bcd --yes`, `zd1 dfu start`). Both need our MCU firmware: it chooses the system slot
(`docs/module/storage.md`). A speaker with empty slots boots into the rescue and waits for its first `zeppelin
update`.

```
nix build -o emu/img/os .#os-image            # the images (first time: the toolchain, ~15 min)
python3 emu/zemu run --keep                    # boot it in the emulator; nc 127.0.0.1 4555 = a shell on the module
python3 emu/zemu run -q --sh 'ls /run'        # or script it: boot, run commands, print their output, stop
nix develop .#cross                            # the cross toolchain: cargo module -p zd1
```
`zemu` builds the image before it boots (nix rebuilds only the pieces whose sources changed), so the loop is:
edit, `python3 emu/zemu run ...`, look. A dev boot takes ~8 s to the shell; `--mcu` adds our MCU firmware on the
link, `--nand` boots the flashable image through BL1, `--trace` / `--gdb` / `--wav` show what the drivers do
(`emu/README.md`).

## Pieces

| | | |
|---|---|---|
| Toolchain | `nix/default.nix` (`flake.lock` pins it) | nixpkgs' LLVM cross toolchain for `armv5tel-unknown-linux-musleabi` (clang/lld 21, compiler-rt, static musl 1.2.5, `-march=armv5te`, soft float, no hardening flags; `nix/armv5-compiler-rt.nix`), native on any host; Rust from the top `rust-toolchain.toml` (a nightly + `armv5te-unknown-linux-musleabi`; std built from `rust-src` with our profile and without its backtrace symbolizer, `.cargo/config.toml`) through crane; each piece a `callPackage` derivation in `nix/module/`, together `nix build .#module-os-parts`; the images `nix/os-image.nix` (`assemble.py assemble`) |
| Kernel | `os.toml [kernel]` | Nokia Kernel_A (`blobs/module/nokia-hd1/kernel.bin`, provenance in `blobs/README.md`; `re/netmodule/NOKIA_HD1.md`), command line |
| Vendor binaries | `os.toml [vendor]` | the kernel's modules (Wi-Fi, AV, sound) and the Wi-Fi firmware from Nokia's rootfs, and the insmod order |
| busybox | `os.toml [busybox]`, `busybox.config` | pinned tarball (sha256), static, twice: the system's (`applets`, 106) and the rescue's (`rescue_applets`, 27), every other applet off (`nix/module/kconfig-merge.py --applets`); `busybox.config` = our changes on top of `defconfig` |
| Apps | `apps/` (Cargo workspace), `os.toml [apps]` | std Rust, installed as **one executable** (`apps/zd1`, busybox style: each program a library's `main`, picked by the name it is started as, a link per program, or by `zd1 PROGRAM`), twice: the core build in the rescue (`/bin/zd1`, `--no-default-features`, nix's `apps-core`: `init`, `zd1-linkd`, `zd1-ctl`, `zd1-board`, `zd1-persparam`, `zd1-settings`, `zd1-slot`, `zd1-update`) and the full one in the system (`/usr/bin/zd1`: `os.toml`'s `core` linked from `/bin`, the rest from `/usr/bin`; relative links, so a slot mounted elsewhere resolves into itself). One copy of std (and alsa-lib) for all of them: separate binaries each carried their own (3.4 MB more). `apps/zd1-compat` (linked into all of them) supplies what the kernel lacks (below), `apps/zd1-sys` what they share (the kernel log, system calls, the MTD ioctls, CRC-32, hex, MACs). `zd1-ctl` is the running system from a shell (init's services, the slots, `zd1-linkd`'s app socket); `zd1-selftest` (the debug build) checks clocks, threads/atomics, futex waits, TCP, files, processes, randomness, /proc; `zd1-persparam` reads the unit's MAC from PersParam (NAND, read-only): eth0 = MAC0, Wi-Fi = MAC0 locally administered; `zd1-entropy` seeds the kernel's entropy pool at boot (init's first service); `zd1-slot` writes the slots; `zd1-update` takes an update from the MCU over the console UART (`docs/module/updater.md`); `zd1-settings` the settings |
| Storage, the boot | `docs/module/storage.md`, `apps/zd1-init`, `apps/zd1-slot`, `os.toml [kernel] cmdline` | the NAND in partitions (`mtdparts`): the vendor area read-only, two **system slots** (JFFS2, each a whole root: the kernel mounts one as `/`, read-only) and the **data partition** (JFFS2 at `/data`). The MCU chooses the slot and the boot stub puts it on the command line (`root=`, `zd1.slot=`); when no slot gets ready, the stub starts the **rescue** (the app partition's initramfs: the MCU link, a shell; it waits for `zd1 update`). `zd1-slot` writes a slot (erase, unpack, stamp) |
| Settings | `docs/module/settings.md`, `apps/zd1-settings` | the name, the Wi-Fi network and address, which receivers run: `/data/zd1/settings`, set from the host through the MCU (`zd1 settings`) or `zd1-settings` on the module; a change restarts what uses it (init: each service lists its settings) |
| MCU link | `module/link`, `module/boot`, `apps/zd1-linkd` | the protocol library; the bare-metal boot stub on it (status booting, the MCU's slot choice, ATAGs); `zd1-linkd`, the link under Linux (status ready, HCR 0x21 -> BL2); the module's log is its console UART, which the MCU logs (init: console level 7) |
| ALSA | `os.toml [alsa]` | alsa-lib (static, in the apps) and its config; `aplay`, `speaker-test`, `amixer` in every system; Nokia's `snd-sulu` is the card (`hw:0,0` = AV port 2 -> the speaker's DSP) |
| Receivers | `apps/zd1-snapcast`, `apps/zd1-airplay` (on our shairplay-rust fork), `apps/zd1-spotify` (on our librespot fork; `forks.md`), `apps/zd1-audio`, `apps/zd1-mdns` | Snapcast, AirPlay 1/2 and Spotify Connect, started at boot (sections below); `zd1-audio` = the receivers' toolkit: the playback device, the output (AirPlay, Spotify), the speaker link, the command line and log; `zd1-mdns` = one-shot mDNS lookups (snapclient) |
| Debug services | `apps/zd1-init` (`debug`), `apps/zd1-memsh`, `os.toml [image.debug]` | a login-less shell on telnet 2323 and, in the debug system (`system-debug.cpio.gz`: zd1's `debug` build, with the self test, PersParam's raw reads and the MCU link's request trace), `zd1-memsh` on 23, which answers the stock KnOS shell's `wr`/`rd` on `/dev/mem`, so `tools/zeppelin/warp` (Warp and Sol probes) runs unchanged against our OS. A root shell and raw memory for anyone on the network: **off** in the normal image, on by default in `os-debug.bcd` (`zd1.debug`; otherwise the same image), and switched only from the speaker's USB port: `zd1 debug on\|off\|default` (the MCU keeps it in RAM and tells `zd1-linkd` at every ready, SPI reg 0xC1). The dev boot (`zemu run`) is the debug system with `zd1.debug` |
| Identification | `/etc/os-release`, SPI reg 0x25 | `VERSION_ID` (the system: the commit, `-dirty` with uncommitted changes; the rescue: 12 hex digits of its contents' SHA-256, so it changes only when the rescue does), `VARIANT_ID` (release or debug: init writes `/etc/os-release` -> `/run` from the system's `/usr/lib/os-release` and the command line; `rescue` in the rescue), `BUILD_DATE` (the commit's; the system only); `zd1-linkd` hands "zd1-os VERSION KIND" to the MCU (`zd1 info` shows it) and keeps the MCU firmware's version in `/run/zd1/mcu-version` |
| The board | `module/dm870`, `apps/zd1-board` | the DM870 by name: every block, register, field and rule our code uses, in one crate (`no_std`; `/dev/mem` under Linux), used by the boot stub, the link, init and the tools. `zd1-board up` is what the Nokia kernel leaves unset (the pad matrix, the PHY's reset and clock, the clock gates; init runs it once at boot); `zd1-board show` prints the chip's state by name |
| Init | `apps/zd1-init`, `apps/zd1-svc`, `apps/zd1-net` | PID 1, ours (`/sbin/init`, the rescue's `/init`): the early start, then the services from one table (`services.rs`), supervised; `zd1-svc` is its socket's client, `zd1-ctl` asks it from a shell (`status`, `start`/`stop`/`restart`); `zd1-dhcp` is udhcpc's event handler |
| Overlay | `rootfs/` | config (`/etc/profile`, `hostname`, `passwd`, `mdev.conf`); copied over everything else in the system |
| Wi-Fi | `os.toml [wpa]`, `wpa_supplicant.config`, `apps/zd1-init` (`wifi`), `apps/zd1-net` (`wpa`) | `wpa_supplicant` 2.11 and `wpa_cli`, static and stripped (509 + 124 KB; WEXT, WPA2-PSK, internal crypto). The `wifi` service runs the supplicant whenever the radio is on (`wifi.mode`): with known networks (`zd1 wifi`, `/data/zd1/networks`) it joins the preferred one it sees, with DHCP on `wlan0` or a fixed address, and a new address restarts `zd1-airplay` so it advertises there; without one it idles and scans when asked. `zd1-linkd` reads its status and asks for scans over its control socket (`zd1-net`'s `wpa`): the State record's `wifi`, `wifi_ssid`, `bssid`, `signal` and the `Scan` record (`docs/module/settings.md`). No image carries a network |
| Packing | `mkcpio.py`, `mkpayload.py`, `tools/zeppelin/fw/{mkbcd,bcd}.py` | deterministic rescue and system image; payload (`--slots`: the stub asks the MCU); .bcd (the stub, the kernel, the rescue) |

Boot: the stub (from the app partition, or straight from SDRAM in the emulator) asks the MCU which slot -> kernel
with that slot as `/` -> **init** (`apps/zd1-init`, PID 1). Its early start: proc, sys, tmpfs on `/dev` `/run` `/tmp`
`/var` (64 KB, 512 KB, 8 MB, 1 MB at most: RAM, no swap), the device nodes (2.6.24 has no devtmpfs, and `mdev -s`
finds nothing; hotplug works), the MTD nodes, `/etc/os-release`, `/data` (formatted once if it isn't ours), the vendor modules (`init_module(2)`, `sulu_wlan` with
the unit's MAC and the antenna port), `lo`, then the board (`zd1-board up`, after the drivers: what they own stays as
they left it). Then the services, in order (`zd1-ctl status` lists them): `entropy` (waited for), `zd1-linkd`,
`ethernet` (the PHY, 169.254.1.1 at once, udhcpc), `wifi`, `time` (the wall clock by NTP, `ntpd -q`, retried until
it is set: the module boots in 1970; on the unit since 2026-10-25),
`snapcast`, `airplay`, `spotify`, `debug`, `selftest` (with `zd1.selftest` on the command line; waited for), then
`[os] up` and `console`, a shell on ttyS1 (the module's console: `zd1 console`). A daemon that ends is
started again (sooner while it runs a while, later while it keeps ending at once); its log is `/var/log/NAME.log`
(init writes it from the service's output), the older part in `.log.1`: a log that reaches 32 KB, and a daemon's
when it ends (with how), becomes `.log.1`, so a service holds at most 64 KB of RAM. Init's own lines and
`zd1-linkd`'s go to the kernel log (`/dev/kmsg`, a fixed ring; lines cut at 1000 bytes), which the MCU logs and JTAG can
read (`docs/module/jtag.md`). `reboot` / `poweroff` (busybox's) ask init, which stops
everything first. The rescue runs the same init with its own two services (`zd1-linkd`, `console`).

The images (`nix build .#os-image`; `emu/img/os` links to them): `system.cpio.gz` (a slot's root: what `zd1 update`
installs), `system-debug.cpio.gz` (the same with zd1's debug build), `rescue.cpio[.gz]` (the rescue),
`payload_dev.bin` (emulator dev boot: the debug system as one plain initramfs at 0x61000000), `payload.bin` + `os.bcd` (flashable: the stub, the kernel, the gzip rescue), `nand_os.bin` (the
emulator's test unit with `os.bcd` installed and empty slots: `zemu run --nand --mcu --allow-panic` goes A, B, the
rescue; then `--host 'zd1 update --uart'`, `docs/module/updater.md`), and `os-debug.bcd` (`os.bcd` with the debug services on by default).

## Adding things
- **An app**: `cargo new --vcs none --lib apps/zd1-NAME` (a member by the workspace's `module/os/apps/*`): its
  program is the library's `pub fn main()`, a program of the multicall (`apps/zd1`; to run it from RAM, copy `zd1`
  over and `zd1 NAME ARGS`). Name it in the top `Cargo.toml`'s `[workspace.dependencies]`, add it to `apps/zd1` (a
  dependency and an entry in `PROGRAMS`, behind `full` unless the rescue needs it) and to `os.toml [apps] bins` (or
  `core`, and `rescue`). Crates from crates.io work (nix vendors them from the top `Cargo.lock`: any cargo build keeps it current).
- **A C program or library**: a `callPackage` file in `nix/module/` (the scope's stdenv cross-compiles, static;
  `wpa_supplicant.nix` and `alsa-lib.nix` are the pattern), listed in `nix/default.nix` and `module-os-parts`, then
  copied into the system tree in `assemble.py make_system` (or the rescue, `make_rescue`, if it needs it).
- **A service**: an entry in `apps/zd1-init/src/services.rs` (the table's order is the start order): its `start`
  (what to run now, or why not, or when to try again), how it lives (a daemon, or a one-off, waited for or not), where
  its output goes, and the settings that restart it.
- **A setting**: a field marked `set` of the `State` record in the protocol spec (`protocol/host/src/lib.rs`), and
  its key in the `settings` of the services that use it (a change restarts them once it has settled).
- **A register**: in `module/dm870`, named, with how we know it (and `docs/module/memory-map.md`); nowhere else.
- **A vendor module**: `os.toml [vendor] modules` (and `load` if it should load at boot).

## Talking to the speaker: zd1-linkd and /run/zd1link
`zd1-linkd` (`apps/zd1-linkd`, on `module/link`) serves the MCU link to our MCU firmware in its native dialect
(`docs/protocol/sequences.md`, Our OS; the only dialect our MCU firmware speaks): network
status, IP, the name (the settings' `name`, by default "Zeppelin Air XXXXXX" from the MAC in PersParam), our receivers
(the one in front playing makes the speaker switch to the module's input, waking it from standby), the volume as
steps, key presses. Our link starting makes the MCU greet us (0xC3, then the speaker's snapshot 0xD2), at boot and
after a restart of `zd1-linkd`; until then only registers a stock host knows are queued (the MCU bootloader's relay to
BL2 reads nothing else). Apps talk to it over the Unix socket `/run/zd1link`, one text line per message (`zd1-ctl`
does it from the shell):

| App -> zd1-linkd | |
|---|---|
| `state playing\|paused\|stopped [SOURCE]` | play state to the MCU (0xC6: the receiver in front and its state). The newest `playing` source owns the speaker: the other apps get `yield SOURCE`, then `resume SOURCE` when it stops or pauses; a non-owner's stop / pause doesn't reach the MCU while another source plays. The receiver in front and its state go to the MCU on 0xC6 (its LED: a colour per receiver; the owner while one plays, else the last to pause until it stops). zd1-snapcast = `snapcast`, zd1-airplay = `airplay`, zd1-spotify = `spotify`, no name = `app` |
| `intent SOURCE` | a person picked this receiver (AirPlay: the sender's first SETUP; Spotify: picked in the app): it goes in front, the others pause, and the MCU switches to the module at once, pulses the LED in its colour and starts the amp (0xC0) |
| `session on\|off SOURCE` | a sender is connected (an AirPlay client, a Spotify Connect session, a snapserver) / gone: the speaker lists the receiver as a source (0xC6 [2]; an app that goes away takes its sessions with it) |
| `volume 0-100` | volume to the MCU, as a share of the range a module may use (0..the cap, 0xD2; sent as a step on 0xD4): applied while the module is the input, else when it becomes it (a later knob change wins); the other apps get it too |
| `log TEXT` | a line in the kernel log, so on the console and in the MCU's log (`zd1 log`) |

| zd1-linkd -> apps | |
|---|---|
| `yield SOURCE` / `resume SOURCE` | another source took the speaker: let go of the audio device / it is done (ignore your own name) |
| `audio on\|off` | the module is / isn't the speaker's input (0xD2 [3]); `off` after `on`: the speaker left us (another source, standby): pause a sender that plays |
| `key playpause\|next\|prev SOURCE` | a key a person pressed while the module is the input (0xD3: the speaker's keys, the remote, a USB host's media keys), for the receiver in front only (else the only one with a sender; else dropped): a key for everyone would reach every connected sender. Entering and leaving the input aren't keys (`audio`, `select`) |
| `select SOURCE` | chosen on the speaker (the ON/OFF key, the play key in standby, `zd1 play airplay`; SPI 0xC2): resume your sender (the others get `yield SOURCE` first and pause theirs) |
| `volume 0-100` | the module's volume, as a share of 0..the cap: on a connect, and when it isn't what the apps asked for (the knob, the remote, the cap); an app's `volume` reaches the other apps |
| `net up IP\|down` | the network: the address the speaker is reached at, a wired lease, else the Wi-Fi's, else a cable's link-local one (eth0's fallback without a cable is no network); also sent to the MCU: 0x32, 0x38-0x3A |

A new connection first gets the current `audio`, `volume` and `net` (known from the MCU's hello on). Seen end to end
with our MCU firmware in the emulator [EMU 2026-10-28, `zemu run --mcu`: the hello at boot and after `killall
zd1-linkd`, keys on the input, a request for 100 % at the cap (step 40), the knob at 60 shown as 100 %, leaving to USB,
the play key in standby choosing the receiver in front].

## Snapcast: zd1-snapcast
Started at boot (init's `snapcast` service; options in `/etc/snapcast.conf` as `SNAPCAST_OPTS=...`, off with
`zd1.nosnapcast` on the kernel command line; log in `/var/log/snapcast.log`). It holds the audio device only while
a server is connected (closed 5 s after it goes), so `aplay` and other apps can use it otherwise.
`zd1-snapcast [--host HOST] [--port 1704] [--latency MS] [--name NAME] [--fixed-delay] [--no-warp0] [-v]`
(`apps/zd1-snapcast`) plays a snapserver's stream (PCM or FLAC, resampled to 44.1 kHz) in sync through ALSA (zd1-audio's
device) and reports to zd1-linkd (zd1-audio's speaker link): the speaker
switches to the module when the stream starts, the server's volume and the speaker's knob follow each other (a
session starts from the speaker's, when the speaker has told it), the
speaker's play/pause, next and previous keys go to the stream over snapserver's JSON-RPC port (stream port + 1 =
1705, `--rpc-port`; `Stream.Control`, for sources that take it: librespot, MPD, ...); for a stream that can't be
controlled play/pause mutes locally. Without `--host` it finds the server over mDNS (`_snapcast._tcp`, what
snapserver announces through Avahi/Bonjour; one-shot queries, answered unicast, asked on each interface) and looks
again after each session; a server on the wired link's subnet comes first. Name: "Zeppelin Air XXXXXX" from the MAC,
like stock.
- **Playout delay**: the server's buffer (less the client's latency) is the floor; the client adds a margin for its
  network (none on the wired link, 300 ms on Wi-Fi to start with). A stall (the queue dry mid-stream, then the rest
  of the stream arriving) grows the margin by its length plus 150 ms and play goes on where it stopped: one gap, no
  audio lost, the next stall that long absorbed. snapserver stamps the first chunk after a break in its input as if
  there had been none (then jumps its clock: `resync`); that chunk counts as stale, not as a stall, when the next
  one jumps ahead. While nothing plays (3 s without sound) the margin is set again from the latest arrival of the
  last 10 min. Stalls, margin changes and realignments are logged. `--fixed-delay`: no margin
  (multi-room with other clients).
- **Decoding**: in its own thread (the reader timestamps the server's time replies; it must not wait on a decoder).
  FLAC on Warp0 (module/warp0, the FLAC image) while the stream has sound and the speaker is ours: taken at the first
  sound, let go after 3 s without (AirPlay's and Spotify's decoders want it too); the same decoder's ARM build
  (`zd1-flac`, with FLAC's own limits) otherwise and for anything Warp0 doesn't decode (FLAC has no state between
  frames: no hand-over). `--no-warp0` keeps FLAC on the ARM. PCM over Wi-Fi logs a hint: `codec = flac` on the server sends about half the bytes.
- **On the unit** [HW 2026-10-29]: found a snapserver over mDNS and played its stream, the server's volume setting the speaker's; a stall stretched with nothing lost (a 1.5 s stall:
  0.98 s of silence, 0 frames lost); Warp0 decoding the live FLAC stream, the same as an ARM decoder over 2,000
  chunks; the whole client 3.8 % of the ARM with Warp0, 15.2 % with our decoder on the ARM (`soak`, 44.1 kHz stereo
  FLAC level 2). The handover with AirPlay and the keys reaching the server: emulator only [EMU].

## AirPlay 1 and 2: zd1-airplay
`zd1-airplay` (`apps/zd1-airplay`) is the receiver: our app on **shairplay-rust**, a pure-Rust AirPlay library
forked and changed for this CPU (`forks.md`: an integer audio path, compressed
buffering, pairing fixes), with our own fixed-point AAC-LC decoder (`module/codecs/aac`). AirPlay 1 (RSA, AES-CBC, ALAC), AirPlay 2 (transient pair-setup
with PIN 3939, or `--pin`; ChaCha20-Poly1305; realtime ALAC, buffered AAC (the iPhone's music mode) and buffered
ALAC, 24-bit kept 24-bit), FairPlay
fp-setup, its own mDNS and PTP listener. Init's `airplay` service starts it (restarted if it exits; off with `zd1.noairplay`),
named "Zeppelin Air XXXXXX" from the MAC like the stock module; log `/var/log/airplay.log`.
- **Output** (`apps/zd1-audio`'s `output.rs`, shared with zd1-spotify): ALSA `hw:0,0`, S32 at the stream's own rate (no resampler: rubato needs 13x real time
  here); samples queued as the stream delivers them (16-bit; 32-bit while a 24-bit stream plays; at most 3 s),
  written four periods at a time, 250 ms prebuffer; the device is
  closed 3 s after the last sample so other sources can have it.
- **The speaker** (`apps/zd1-audio`'s `speaker.rs`, through `zd1-linkd`): `state playing|stopped airplay` (reported before the device
  opens, so zd1-snapcast lets go first), the AirPlay volume as `volume 0-100` (the speaker sets the level; samples go
  out at full scale), the speaker's keys to the sender (AirPlay remote control: play, pause, next, previous),
  `yield` from another source pauses the sender; a source still playing when the speaker comes back to it (`resume`,
  `select`) says `state playing` again.
- **On the unit** [HW 2026-10-24 on]: from a Mac (Music, the system's audio) and an iPhone, wired and over Wi-Fi,
  in step with a Mac's own speakers; CPU (`debug/soak.c`: the kernel's accounting misses this load, below) 11 % for
  buffered AAC, 28 % for AirPlay 1 ALAC, 26 % for AirPlay 2 realtime ALAC, all decoded on Warp0; peak RSS 2-3 MB.
  `docs/streaming.md` has the breakdown and the sync.
- **Warp0** decrypts and decodes the realtime ALAC streams and decodes buffered AAC and ALAC (16- and 24-bit)
  (`module/warp0`, as shairplay-rust's `Decoder` for the stream): the firmware is built into the binary (nix's `warp0-fw`, `module/warp0/build.py`), loaded at each
  stream's start, parked at its end; a hang or anything it can't take falls back to the ARM. `--no-warp0` off.
  Test hooks: `ZD1_PCM_HASH[=N]` logs a hash of the (first N) samples per stream, `ZD1_PCM_DUMP=FILE` writes them.
  The CPU it takes is measured with `debug/soak.c` A/B runs: the sampling profiler (`debug/prof.c`, symbolised by
  `debug/profsym.py`) runs on the kernel's 10 ms tick, like its CPU accounting, so it misses short bursts that start
  after a tick and end before the next (a stream's per-packet and per-frame work); it is for CPU-bound code (the
  decoder benches).
- **Pairing**: the accessory's identity and up to 16 paired controllers in `/data/zd1/airplay` (the same accessory
  across restarts); without a PIN, pair-setup is transient (per connection).

## Spotify Connect: zd1-spotify
`zd1-spotify` (`apps/zd1-spotify`) is the Spotify Connect receiver: our app on **librespot** 0.8 (Connect, the
session, the player), four crates of it patched in our forks (`forks.md`: a decoder plug-in and integer samples in
the player, a bounded memory window in the downloader, IPv4 and no-SO_REUSEPORT fixes for discovery, stopping
instead of skipping or hanging on tracks that don't play). Spotify's
Ogg Vorbis goes through our fixed-point decoder (`module/codecs/vorbis`), on Warp0 when it is free (the Vorbis image,
`module/warp0`), else on the ARM; rustls + ring for TLS (certificates need the clock: init's `time`). Init's `spotify`
starts it (restarted if it exits; off with `zd1.nospotify`), named like the AirPlay receiver; log
`/var/log/spotify.log`. The speaker shows up in Spotify apps on the same network; picking it sends the
account's credentials (zeroconf), kept in memory only: after a reboot it is picked again.
- `zd1-spotify [--name NAME] [--device hw:0,0] [--bitrate 96|160|320] [--window-kb KB] [--no-warp0] [-v]`; tests:
  `--play-file FILE.ogg [--seek-ms MS]` (the decoder and output path without Spotify; `ZD1_PCM_HASH`,
  `ZD1_PCM_DUMP` as above).
- **Output and speaker**: zd1-audio's, as zd1-airplay: the player is held 500 ms ahead of the speaker; Spotify's
  volume goes to the speaker as `volume 0-100`, samples at full scale; the speaker's keys and `yield` go to
  Spotify (play, pause, next, previous). Source name `spotify`.
- **Tracks that don't play**: a refused audio key stops the player on the track (the app shows it paused; play,
  play/pause or a new pick try it again), said once per session in the log; other load failures skip the track,
  five in a row stop playback. A new pick waits at most 5 s for the previous session to end.
- **On the unit** [HW 2026-10-25, 2026-10-28]: the decode-and-output path 11.5 % of the ARM with Warp0 (30 %
  without), the output the host decoder's sample for sample; discovery, the pick and the login work; Spotify refuses
  the test account every audio key, and each pick, play and next is then one load, paused in the app, the next
  pick answered at once. `docs/streaming.md` has the details.

## Why busybox
Two Rust replacements were tried on this kernel in the emulator and dropped: armybox (quinnjr/armybox) and busybox-rs
(rebizzz/busybox-rs). Both ran without crashes or missing syscalls, but neither can be the system: armybox has no
init/mdev/udhcpc; busybox-rs needed a 32-bit port, its `ash` has no expansions or control flow and its `awk` doesn't
run programs; both lack a `dmesg` that works before Linux 3.5.

## Constraints that come from the kernel
- **2.6.24.7, ARMv5TE, EABI, no source.** glibc refuses kernels that old; musl uses the old syscalls when the new
  ones return ENOSYS (time64, getrandom, statx, ...). Rust's std works on it (the self test checks).
- No devtmpfs (`mdev -s` fills /dev), no cgroups/namespaces/inotify-era features, `HZ=100`, no IPv6.
- **System calls Rust programs need that it lacks** (`apps/zd1-compat`, linked into every app with `--wrap`):
  `FUTEX_WAIT_BITSET` (2.6.25), which std uses for every blocking wait (Mutex contention, Condvar, park, channels):
  without the shim each wait returns at once and the thread busy-loops; `epoll_create1` and `eventfd2` (2.6.27),
  which tokio needs to start. Rust's `libc` uses a 32-bit `time_t` on 32-bit musl, musl 1.2 a 64-bit one: the shim
  passes the kernel the old layout. musl's `sched_setscheduler()` is a stub (ENOSYS): use the system call.
- **No entropy**: the pool reads 0 for good (no driver credits it) and there is no `getrandom(2)`; whatever waits for
  `/dev/random` (Rust's `OsRng` via the `getrandom` crate, libsodium) waits forever. Init's `entropy` credits ~2000 bits
  of CPU timing jitter at boot (`zd1-entropy`; ~6.6 bits of min-entropy per sample measured, a quarter credited).
- **CPU accounting is tick-sampled and misses most audio work**: a thread that wakes on a tick and sleeps before
  the next is never charged, so `top`, `/proc/PID/stat` and even `/proc/stat` idle under-report a decoder by 2-10x.
  Measure what is left over instead: `module/os/debug/soak.c` (a `SCHED_IDLE` spinner; load = 1 - rate / idle rate,
  idle rate ~21,950 loops/s).
- Its idle loop doesn't sleep (`arch_idle` is empty): fine in the emulator (patched to WFI there), a power topic on
  the unit. `udelay`/`mdelay` busy-wait.
- Memory: 32 MB, Linux gets 31 (`mem=31M`: the top MB holds the boot stub, then the Warp0 runtime's window and the
  Warp1 program window). The system is a slot (JFFS2) mounted as `/`, read as it is used (`docs/module/storage.md`);
  only the rescue, and the emulator's dev boot (~0x61000000 up to the stub at 0x61F00000), are initramfs in RAM.
- The kernel's own NAND reads of the vendor's pages are miscorrected by its soft ECC: read those raw
  (`zd1-persparam` does); the vendor partition is read-only to the kernel. Our partitions are written and
  read through MTD with the kernel's own ECC (`docs/module/storage.md`, `docs/module/boot-chain.md`).
