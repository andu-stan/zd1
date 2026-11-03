# Updating the speaker over USB

How the speaker gets new firmware, MCU and network module alike, over USB alone: no Ethernet, no Wi-Fi, no JTAG.
The flows, the design, what has been measured on the unit, and what is open. Tags: `docs/README.md`.

## Goals
- **Everything over USB**, through the vendor interface (WebUSB-reachable), the module's console included: the
  speaker has no serial port. The same flows from a terminal (`zd1`) and
  from a web page (WebUSB): one implementation, `host/core` (below).
- **Smooth**: no step that needs a person except the very first DFU entry on a stock speaker (hold ON/OFF at
  power-up). No power-cycling by hand, no waiting that a tool can't explain.
- Ethernet (BL2's web updater) is not part of any flow.

## The flows

| When | Path | Time | State |
|---|---|---|---|
| **first install** on a stock speaker | **`zd1 install`** (`host/core/src/install.rs`, the page's Install): key-hold DFU → the relay of `os.bcd` (stub, kernel, rescue) → MCU firmware over DFU with a backup → start → the module's first boot (slots A and B empty → the rescue) → the system into a slot, started once. `--no-bcd`: the boot image is there (a repair) | ~5 min (the relay 85 s, the first boot up to ~3, the update ~1.5) | the flow without the relay on the unit (2026-10-31, `--no-bcd`); each step with the relay on the unit (2026-10-26); the whole flow with the relay: not yet |
| **back to stock** | **`zd1 rollback STOCK.bcd STOCK.hex`**: the same order, the stock `.bcd` through the relay (6.5 min), then the stock MCU firmware (or a backup's `app_flash.bin`) with a backup first; the speaker comes up unconfigured | ~7 min | the pieces on the unit (`docs/recovery/factory-reset.md`); as one flow: not yet |
| **MCU firmware** | host link `Dfu` → DFU flash → start (`mcu/flash.sh`, a backup into `units/<unit>/mcu/dfu-backups/` first) | ~15 s | in daily use |
| **module: stub, kernel or rescue** (rare) | `zd1 xmodem IMAGE.bcd`, our firmware driving BL2's XModem, then `zd1 module restart`; or the relay, started from our firmware with `Dfu` (no key-hold): `zd1 bootloader`, `zd1 dfu relay IMAGE.bcd --yes`, `zd1 dfu start` | 305 s (XModem, 2 MB); the relay 85 s | both on the unit: XModem 2026-10-27; the relay's full install 2026-10-26, its handover to BL2 against our OS (2e20f8d on) 2026-10-27 |
| **module: everything else** (routine) | **`zd1 update`**: the image over the vendor interface's bulk endpoint → MCU → module UART at 1 Mbaud, in frames → `zd1-update` writes the slot that doesn't run → started once, kept if it gets ready | 82 s for 5 MB framed at 1 Mbaud (61 KB/s) | in daily use (2026-10-26 on) |
| BL2 broken | BL1's menu 1 ("Download Bootloader Level 2"), XModem over the UART bridge (`blobs/module/bl2.bin`, 799 KB) | ~1.5 min | untested anywhere; BL1 has none of BL2's console hotkeys (`strings`); writes a protected region: the unit owner's OK each time |
| BL1 broken | JTAG (`docs/module/jtag.md`); none of our tools writes BL1 | | |

## Our updater: the design

The app partition (vendor format: BL1 checks MetaInfo and CRCs, the vendor's NAND ECC) stays as it is and changes
rarely. Everything that changes often lives in **system slots** on our own MTD partitions, which Linux can write.
How the module boots from them: `docs/module/storage.md` (The boot).

```
app partition (BL2 / relay only): boot stub + the kernel (deflated) + the RESCUE initramfs (1.8 MB of 10 MB)
      stub: asks the MCU (reg 0xC4) -> root=/dev/mtdblock1|2 (a slot), or the rescue
NAND 32-64 MB  system-a  (mtd1)    a whole root: busybox, zd1, /etc, /usr
NAND 64-96 MB  system-b  (mtd2)
NAND 96-128 MB data      (mtd3)    settings
MCU EEPROM: the good slot, a slot to try once; RAM: the boots that failed since the last ready (mcu/src/modboot.c)
```

Data path:
```
host ──EP0 (host link)── MCU ──SPI reg 0xC5── zd1-linkd → zd1-update     control: Update BEGIN, UpdateStatus
host ──bulk OUT 0x05──── MCU ──USART1, the baud─── ttyS1 → zd1-update     the image in frames, four in flight
                              ◄── 0E SEQ (good) / 15 SEQ (again from SEQ); 0x02 lines: ready / ok / error
```
1. **`Update` BEGIN** (gated: size, SHA-256, the stamp, baud; `mcu/src/update.c`): the MCU writes reg 0xC5 to the
   module, takes USART1 from the console passthrough at the baud (`modcon_take`; the divisor in eighths: 1 and 2
   Mbaud exact, 115200 / 230400 / 460800 within 0.16 %; a baud it can't make within 2 % is refused) and points
   the bulk pipe at itself (sink `MODULE`, from zero). `zd1-linkd` starts `zd1-update SIZE BAUD SHA256 ID`
   (`module/os/apps/zd1-update`, in the core: the system and the rescue have it).
2. **`zd1-update` takes the console**: the processes reading it stopped (the shell), the kernel's console level 1,
   ttyS1 raw at the baud with the low-latency flag; `0x02 zd1-update ready SLOT \n`. SLOT: the one that
   doesn't run (from the rescue: the good one).
3. **The image**: the host writes it to the bulk endpoint (the USB side NAKs while the pipe is full). The MCU cuts
   it into frames, `7E SEQ LEN(2) DATA(≤ 512) CRC32(4)` (little-endian; the CRC over SEQ, LEN and DATA), and keeps
   four in flight (2 KB: J2 has no RTS/CTS and Linux's tty buffer is 4 KB). The module reads a frame when it wants
   data and answers `0E SEQ`; a bad CRC or length, a frame that isn't the next, or 500 ms of nothing gets `15 SEQ`
   (the frame it wants), and it skips to that frame's start. The MCU keeps each frame until answered and sends
   again from the NAK'd one (go-back-N). Once the first frame is in (an MCU that missed the BEGIN's echo gave up
   and sends nothing: the slot stays as it was), `zd1_slot::install` erases the slot, inflates and unpacks as the
   bytes come, reads the gzip stream to its end (its CRC), checks the SHA-256 of everything, then writes the stamp.
4. **The end**: `0x02 zd1-update ok SLOT ID \n` (or `error WHY`: the slot holds no system); both ends back to
   115200 and the shell continued. What `zd1-update` takes of the console is written down as it takes it
   (`/run/zd1/update-console`): if it dies without giving it back (killed, a panic), `zd1-linkd` does once it has
   reaped it. `zd1-linkd` runs one update at a time. `UpdateStatus` says DONE and the slot.
5. **Start it once**: `Boot` TRY the slot with `restart`. Ready makes it the good slot; a boot that doesn't get
   there (a panic restarting the module, or no ready in 90 s: the MCU power-cycles it) gives the old one back
   (`storage.md`).

`zd1 update [IMAGE]` (`host/core/src/update.rs`; the web page runs the same) does all of it with `system.cpio.gz` from the build: the
stamp is `VERSION SHA256[:12] LENGTH` (the version from the image's `usr/lib/os-release`), and it waits for the
verdict of step 5. Timeouts: the module gives up after 10 s without data, the MCU after 15 s without an answer (60 s
after the last byte for the result), and at once when the module is powered off or restarted (`zd1 module
off|restart`, a slot change with `--restart`); either way the slot holds no system and the running one is untouched.
`--no-start` installs only. In the emulator, whose USB carries EP0 only, the image goes in `PipeData` requests
(240 bytes each); `--uart` skips the MCU's part entirely (the console socket stands in for USART1).

## The web page
What it is to become, the pairings and the interlocks: `docs/web.md`.

`host/web`: `host/core` compiled to WebAssembly, over WebUSB through nusb's browser backend (the same crate the CLI
uses natively), and a static page (`www/`): the speaker's state, source and volume, its name, Wi-Fi (the WPA2 key
derived in the page), address and receivers, the system update and the MCU firmware update, the MCU's log. `nix build
.#web` is the page as a directory; `python3 tools/zeppelin web` serves it on localhost (WebUSB wants a secure origin:
localhost or HTTPS) with this checkout's built images offered for install. A browser with WebUSB: Chrome, Edge, Brave,
Opera on a desktop.
- The first connect asks the browser's chooser for the speaker (03EB:2FE0, or the DFU bootloader 03EB:2FF8); an MCU
  update asks once more for the bootloader, a device of its own (the page shows why). Granted devices are remembered.
- A speaker in its bootloader (asked to, ON/OFF held at power-up, or firmware that doesn't start) gets its own view:
  how far the firmware's last boot got (the boot log, read through DFU) and the module's state as the bootloader sees
  it; start the firmware (`flash::start`, as `zd1 dfu start`), or install MCU firmware from there (the same flow with
  the backup first). In the emulator the page's side of it ran against the bootloader [EMU] (`zemu run --mcu --host
  'zd1 bootloader' --host 'node host/web/test/emu.mjs "$(nix build --no-link --print-out-paths .#web)"'`).
- The gate is the speaker's: the page shows "press ON/OFF on the speaker" when a request comes back LOCKED.
- The MCU update's backup (the app region, the user page and a manifest: `host/core/src/backup.rs`) is one tar, the
  page's download and `zd1`'s file in `units/<unit>/mcu/dfu-backups/` alike, kept before anything is written (the
  flash waits for it): `zd1 dfu restore BACKUP.tar`, `zd1 flash BACKUP.tar` or the page's "Install MCU firmware"
  take it back.
- Windows needs WinUSB on the vendor interface (MS OS 2.0 descriptors: not in the firmware yet, `backlog.md`); macOS and
  Linux need nothing (Linux: a udev rule for the user's access).

## On the unit
- **The bulk pipe**: vendor interface IF3, bulk OUT **EP 0x05**, 64-byte packets, two banks (one until 2026-11-02: the
  same speed, 903-916 KB/s over 4 and 16 MB with two [HW 2026-11-02, feffd90]: the MCU emptying its 512-byte buffers
  is the limit, both full ~1600-2000 times in 16 MB); `zd1 bulk-test
  [MB]` (sink `DISCARD`: counts + CRC-32): 930 KB/s, CRC right from 64 bytes to 16 MB; ~810 KB/s while USB audio
  streams and switches mid-transfer [HW 2026-10-26]. A transfer whose length is a multiple of 64 but not of 512 ends
  with a zero-length packet; `Pipe reset` starts clean.
- **The USB endpoint RAM** (`mcu/config/conf_usb.h`): under 1 KB; audio 512 bytes, the pipe 128 (64 × 2), HID and
  USB audio's status interrupt 8 each, EP3 and EP4 free (the CDC port had them until 2026-11-02). Audio is on EP 0x06 *above* the pipe: ASF re-allocates every endpoint
  above one whose alternate setting changes, and macOS switches audio's while playing (with the pipe above audio a
  switch lost a 512-byte buffer). Probing a layout by hand: allocate in `UECFGn`/`UERST`, read `UESTAn` bit 18
  (CFGOK), undo; never while that endpoint is live (restoring a live endpoint by hand broke the pipe until the next
  flash: ASF also sets a bank-switching flag).
- **The module UART** (`docs/hardware/cr870-module.md`, UART speed): ~80 MHz clock, 1 and 2 Mbaud exact; the MCU's
  divisor in eighths makes 115200 / 230400 / 460800 within 0.16 %. To try a rate by hand: the module with `stty -F
  /dev/ttyS1 BAUD raw -echo` **in the foreground** (a background job that changes its own terminal gets SIGTTOU and
  stops), the MCU with `zd1 poke -w 0xFFFF1820 <BRGR>` (26 = 115200, 3 = 1 Mbaud, 0x40001 = 2 Mbaud), and
  always a `timeout` on the receiving side so it returns to 115200.
- **`zd1 update`**: 5 MB framed at 1 Mbaud in 82 s (61 KB/s; 871 of 9922 frames sent again), at 460800 in
  123 s (7 again) [HW 2026-10-26]. Unframed (the image as it is, paced by the module's grants) took 226 s at
  230400 and failed every time at 1 Mbaud, 5-6 s in: why the frames.
- **A failed or aborted update leaves both boards running**: an ABORT with 192 KB streamed and the pipe full, and a
  corrupted 1 Mbaud stream, both answered and the MCU went on [HW 2026-10-26]. (ASF's `udd_ep_abort()` frees busy
  banks with KILLBK, which never frees an OUT bank: our copy hands an OUT endpoint's busy banks back instead,
  `mcu/asf/avr32/drivers/usbb/usbb_device.c`, marked `zd1:`.) `zd1 update` stops writing when the update ends
  early (`UpdateStatus` between 64 KB chunks); Ctrl-C sends ABORT.
- **The module's console** goes to the UART from the TXRDY interrupt (from the main loop it moved 1 KB/s).
- **The slots**: an empty speaker's first boot (A and B fail, the rescue), a slot started on trial and confirmed,
  each update since into the slot that doesn't run (`storage.md`) [HW 2026-10-26 on]; an update through the MCU from
  the rescue (`zd1 slot try rescue --restart`, `zd1 update --no-start`: into the good slot, 5.1 MB in 75 s, then
  `zd1 module restart` back to it) [HW 2026-11-01, rescue 34fb38ea617d, MCU 4f2215a; 2026-11-02, rescue 2bcb3098d4d7, MCU 7840b81, after the relay: 72 s]. A trial that fails and falls
  back: [EMU 2026-10-26] only.
- **The `.bcd`** is 2 MB (the stub, the kernel deflated, the rescue); the stub's inflate takes 2717 ms on the unit
  (D-cache off; 399 ms in the emulator).
- **BL2's paths, for recovery**: the relay (9 MB in 382 s, 23.4 KB/s; `os.bcd` in 85 s); our MCU driving BL2's SPI
  XModem (`zd1 xmodem`, `XmFrame`: HCR 0x2220 starts BL2's receiver; 9.5 KB/s, BL2 itself needs 18 ms
  per KB, so ≤ ~40 KB/s even with our overhead removed; BL2 doesn't reset afterwards: power-cycle). BL2's serial
  XModem (its menu's 1 and 2 over the module's console; ~14 min for 9 MB at 115200; no tool drives it now) is untested
  on the unit; in the emulator our `os.bcd` went through it whole (2076 blocks ACKed, "download passed"; 2026-11-02,
  with a host-link XModem sender that was then dropped for it), then BL2 said "Data not encrypted" and went
  back to its menu without burning it: the app partition untouched, only the CnE blocks (0x60000, 0x80000) written
  [EMU]. So for our images BL2's serial menu is no way back unless the unit's BL2 does otherwise; its web updater and
  the relay are. **BL2 erases the app partition the
  moment a download starts**: never stop one midway (`zd1 dfu relay --max-blocks` warns).
- **The first boot after a `.bcd` install** fills the partitions (~46-61 s to `[os] up`). The MCU waits 180 s for
  it (`Module POWER`: 120 s by default). After a BL2 web burn BL2 resets the module itself: don't power-cycle it.

## Open
- 1 Mbaud loses ~9 % of the frames (`docs/backlog.md`, Updates and recovery).
- `zd1 install` with the relay, and `zd1 rollback`, as one flow on the unit (each piece has run there; `install --no-bcd` has).

Fallback when the MCU's update path doesn't work: the `.bcd` through BL2's web updater or the bootloader's relay
puts the rescue back, and `zd1 update` installs a system from it. (`zd1 update --uart`, `zd1-update` fed over the
console, is the emulator's, for the module alone; on the unit it went over the CDC port, 442 s for 5 MB, until the port
went.)

## Where things are
- MCU: `mcu/src/update.c` (the update), `mcu/src/pipe.c` (the pipe: `pipe_peek`/`take` for a consumer, `PipeData`'s
  `pipe_put`), `mcu/src/hostif.c` (the host link), `mcu/src/modcon.c` (the module's console on USART1), `mcu/src/dm870.c` (module
  power, SPI, `XmFrame`'s `dm870_xm_send`), `mcu/config/conf_usb.h` (endpoints).
- Protocol: `protocol/host/src/lib.rs` (`Update`, `UpdateStatus`, `PipeData`, `Boot`, `Pipe`, `XmFrame`, `Sink`,
  `usb::BULK_OUT`), regenerate with
  `cargo run --manifest-path protocol/gen/Cargo.toml` (`nix develop`); the SPI registers in
  `protocol/spi/src/spec.rs` (`BOOT` 0xC4, `UPDATE` 0xC5, `XM_MODULE` 0x2C, `XM_HOST` 0x2D, HCR values).
- The computer's side, once: `host/core` (`update.rs` the system update, `flash.rs` the MCU firmware, `dfu.rs` the DFU
  bootloader and the relay, `lab.rs` `bulk-test`, `xmodem.rs`, `usb.rs` nusb and the emulator's bridge); its
  front-ends `host/cli` (`zd1`) and `host/web` (the page). `host/core/src/console.rs` (`run`) drives the module's console
  over the host link.
- Module OS: `module/os/` (`os.toml`, `assemble.py`, `apps/zd1-init` (PID 1 of the system and the rescue), `apps/zd1-slot`,
  `apps/zd1-update`, `apps/zd1-linkd`); the stub `module/boot/src/bin/linuxboot.rs`; the MCU's choice
  `mcu/src/modboot.c` (hooks in `dm870.c`: `stub_check`, `on_ready`, the bring-up's deadline).
- The unit: `units/<unit>/status.md` (not in git). Images: `units/<unit>/module/images/`.

## Working notes for the next agent
- The emulator can't carry bulk transfers (its USB bridge is EP0 only): the pipe is tested on the unit only. It does run the real BL1/BL2 (`--nand --real-bl1`), the module console socket for
  `--host` commands (`ZD1_MODCON_EMU`), and our firmware with the gate open.
- Slot tests in the emulator: `zemu run --nand --nand-image emu/run/PREV/nand.bin` boots the previous run's NAND
  through BL1 (without `--nand` it is the dev boot: no slots). A NAND written by `emu/tools/bcdinstall.py` over a
  previous run's keeps that run's `nand.bin.oob`, whose app pages then fail BL1's ECC (BL2 starts): give the app
  region the spare bytes BL2 would leave (`tools/zeppelin/nand/ecc.py` `calc`, as `CR870_NAND.Load` does) or drop
  the `.oob` (then the data partition's JFFS2 doesn't read back).
- An emulator speaker with a system: `nand_os.bin` has empty slots (the `.bcd` carries none), so `zemu run --nand
  --mcu --allow-panic --until 'waiting for a system' --host 'zd1 update --uart'` first (~4 min
  real; the NAND it leaves in `emu/run/NAME/nand.bin` boots slot A from then on, `--nand-image`). Through the MCU
  instead (`zd1 update` without `--uart`) the image goes in 240-byte `PipeData` requests: ~80 s for 5.3 MB.
- A run with slots that fail: `zemu run --nand --mcu --allow-panic` (a panic is part of the story); `--host
  'zd1 slot try b --restart'` drives the MCU, `host.log` has the output. Each run starts from
  the factory EEPROM: the MCU's good slot is A again.
- On the unit: every `zd1 ...` session drains the MCU's log, and only `zd1 --log ...` prints what it drains (`zd1 --log
  update` shows the MCU's side of the update and the boot after it);
  the MCU powers the rescue off 180 s after starting it unless it gets ready; `zd1 console run` waits for a marker it
  echoes, not a prompt, so the rescue's `~ #` is no matter (untested there); the rescue's shell has no controlling tty
  (Ctrl-C does nothing, so never `wait` there) and its busybox has no `killall`/`tail`.
- JTAG (`docs/module/jtag.md`): inside `catch { }` OpenOCD swallows `mdw` output: `echo [capture {mdw phys A N}]`;
  always `resume` in the same session. The reference dumps in `re/netmodule/jtag/`: `regs*-linux.txt` is the
  **broken** state from before the pad-matrix fix, compare a working state against `regs*-bl2.txt`.
- The kernel's console level is 1 by itself; init sets 7 in the system and the rescue (the MCU logs the UART); `zd1-update` sets 1
  while it holds the line and puts the old level back.
- In emulator runs the MCU's trace caps at `pm TraceMaxLines`; `--renode 'zd1:pm TraceQuiet true'` drops polling.
- Flashing either board needs the unit owner's OK (`AGENTS.md`); the amp is allowed from every boot, so keep sound
  short. Commit before a flash so the version stamps name a commit.
