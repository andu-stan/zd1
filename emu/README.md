# Emulator: a development and RE aid for the module OS

An emulated CR870 network module (and, when wanted, the speaker's main board linked to it over J2) in
[Renode](https://renode.io). It boots our module OS (`module/os/`) in a few seconds and gives a shell, a way to run
commands from a script, a trace of what the drivers do to the hardware, a GDB server, the audio output as a WAV,
and the MCU link to our own MCU firmware. It also runs the vendor code (the Nokia kernel and its modules, which our OS
uses as is; KnOS; BL1/BL2), so you can see what the vendor drivers do before you write ours.

It is not a test suite. The models were written by reading the vendor firmware, so the vendor firmware running on
them shows only that the reading is consistent with itself. What counts is the unit.

| Machine | What | Models |
|---|---|---|
| `cr870` | BridgeCo CR870 module: DM870 SoC (Renode's ARM926EJ-S, SDRAM, NAND, VIC, timers, GPIO, UARTs, J2 SPI slave, MACB + a user-mode network, SuluDMA, AV ports, the two Warp MIPS cores) | `renode/cr870/`, `renode/platforms/cr870*.repl` |
| `zd1` | the main board: AT32UC3A1256 (our AVR32 interpreter) + DSP, codec, CS2000, EEPROM, keys, IR, LED, J2 | `renode/avr32/`, `renode/zd1/`, `renode/platforms/zd1*.repl` |

How the models work and how time is kept: `renode/README.md`.

## What the emulator proves
That code does something plausible against models written from the same code and docs. It does not show that the
hardware behaves that way. Every model is someone's reading of the chip, and several readings were wrong until the
unit showed otherwise (the GPIO polarity, the pad switch matrix, the PHY's reset and clock, the 80 MHz timestamp,
the NAND's chip enables, BL1's mailbox handling). When the unit and the emulator disagree, the unit is right: fix
the model. Mark emulator-only results `[EMU]` in the docs.

Where the models are known to be thin (a result there means nothing for the unit):
- **NAND**: the spare areas are synthesized for an image that has none (one write each, the unit's ECC layout:
  `docs/module/boot-chain.md`) and kept next to it as `nand.bin.oob`; the kernel's soft ECC miscorrects normal MTD
  reads as on the unit (`dd` of block 1 through `/dev/mtd0ro` flips "UEnC" to "UE.C" [EMU]); BL1's and BL2's pages
  also get the SmartMedia codes the unit has there. Missing: bad blocks, bit errors, timing.
- **UARTs**: no line rate. Bytes from a socket or the MCU arrive at once and queue (4 KB on the module's side); what
  the kernel's tty layer does with them is the guest's own.
- **Wi-Fi**: the WILD core's 128 KB SRAM is there, the core isn't. `sulu_wlan` times out waiting for its firmware.
- **Warp / Sol**: Sol is KnOS's reference model, which differs from the hardware (`docs/module/warp.md` section 8).
  The Warp cores follow section 3 (memory maps, hangs, ISA quirks, the 2-way random-replacement I-cache, measured
  cycle costs, the Warps slowing each other's SDRAM loads) without the ARM's and DMA's contention
  (`docs/decisions.md`); addresses the unit wasn't probed at read 0 (logged).
- **GPIO**: the block as measured (`docs/module/memory-map.md`); a released pad reads the carrier's pull (low on
  4, 6, 12, 15, 17, high elsewhere), the strap follows MCU PB01 with `--mcu`; registers start at the chip's reset
  values (all pads released, the pad matrix unrouted). The I2C bus holds the auth chip alone, with the
  unit's registers 0-8 (no certificate).
- **USB host**: not modelled. (Its "USB FIFO memory" at 0x40-0x3FFF is the SRAM alias, on the unit too.)
- **Analog and timing**: clocks and PLLs lock at once; no supply, crystal start-up or brown-out; UART baud rates and
  line breaks are not modelled.
- **BL1**: the default is a stand-in that boots the app or BL2 by the documented rules; `--real-bl1` runs the
  decrypted vendor BL1 (slow).
- **USB**: the MCU's USB is a bridge to the host, not a USB host: EP0 only (the B&W bootloader's DFU, our firmware's
  enumeration and host link: `zd1`, `zd1 settings` with `ZD1DFU_EMU` set, `--host`); its other endpoints
  configure and stay idle (no USB audio, HID, status interrupt or bulk pipe; the module's UART is also on a socket,
  4555 with `--keep`, `ZD1_MODCON_EMU` for `--host` steps). **Network**: user-mode
  only (below).

## Start
```
python3 emu/zemu setup                      # Renode (pinned) into emu/renode/dist, the test unit's factory images
python3 emu/zemu run                        # build our OS, boot it to "[os] up", stop
python3 emu/zemu run --keep                 # ... then stay up in real time: nc 127.0.0.1 4555 = the module shell
python3 emu/zemu run -q --sh 'dmesg | tail -5' --sh 'cat /proc/interrupts'
python3 emu/zemu run --mcu --keep           # with the main board running our MCU firmware, linked over J2
```
Needs Python 3.11+, and nix: zemu builds the images it boots with it (`os-image`, `test-unit`, `mcu`
for `--mcu`; `CONTRIBUTING.md`, Builds) and links them into `emu/img`.
`zemu run` rebuilds what changed before it boots (nix rebuilds only what changed), so the loop is: edit,
`zemu run ...`, look. Our OS reaches `[os] up` in ~5 s of virtual time (~8 s on an M-series Mac).

## zemu run [IMAGE] [options] [steps]

| IMAGE | |
|---|---|
| `os` (default) | our OS: the dev payload (the debug system as a plain initramfs, `zd1.selftest zd1.debug`: telnet shell on 2323, `zd1-memsh` on 23) straight into SDRAM; with `--nand` the flashable `os.bcd` from NAND through BL1 and the preloader, like the unit |
| `stock` | the stock KnOS app (its shell on the console) |
| `none` | no module: the main board alone, with its built-in module stand-in on J2 |
| a file | a `.bcd` (installed into the factory NAND like BL2 does, booted from NAND), a NAND image, or a stage 2 for 0x60008000 |

The rest of the unit is the factory state of a test unit (MAC `EC:66:D1:00:00:42`, `tools/zeppelin/recover/mkunit.py`):
NAND with BL1, BL2 and PersParam, the MCU's flash, user page and EEPROM. Nothing per-unit is needed.

| Option | |
|---|---|
| `--nand` | boot from NAND through BL1 (default: the image's stage 2 straight into SDRAM) |
| `--nand-image FILE` | this NAND instead (e.g. a previous session's `emu/run/NAME/nand.bin`) |
| `--real-bl1` | the decrypted vendor BL1 instead of the stand-in (with `--nand`) |
| `--mcu [ours\|stock\|FILE]` | the main board too, linked over J2 like the unit (it powers the module up): our firmware (an emulator build: its log on USART0 too; the host link's gate opened in the run's EEPROM), the stock firmware, or a 256 KB flash image |
| `--trace [PREFIXES]` | the module's MMIO trace in `module.trace`: every access, or those whose address starts with these hex prefixes (`--trace b5006,e0010`) |
| `--gdb PORT` | a GDB server on the module's ARM926 |
| `--wav FILE` | record the AV port audio output (in the run directory) |
| `--patch ADDR=WORD,...` | module RAM pokes at the first MMIO write |
| `--fwd HOST:GUEST` | a TCP port forward to the module (repeatable) |
| `--renode MACHINE:CMD` | any Renode monitor command after setup (below; repeatable) |
| `--keep` | after the steps, stay up in real time until Ctrl-C (or `kill`): module console on 127.0.0.1:4555, MCU USB 4620 (`ZD1DFU_EMU=127.0.0.1:4620 zd1 ...`), ports 8080->80, 2323->23, 2324->2323 |
| `-q` | no live console; print each `--sh` command's output instead |
| `--timeout S` | per step (300 virtual seconds) |
| `--allow-panic` | a kernel panic doesn't end the run: the module restarts (`panic=10`, or the MCU power-cycles a boot that doesn't get ready; the system slots' fall-back, `docs/module/storage.md`) |
| `--name NAME` | the run directory `emu/run/NAME/` (default: the image name; give one per session if you run several at once) |

**Steps** run in the order given; all times are virtual. They start once the image is up (`[os] up`, KnOS's
`System initialisation done`) unless the first step is a wait or `--at-once` is given (for the boot itself: BL1's
serial window). A wait looks at the output since the previous step began.

| Step | |
|---|---|
| `--until RX` | run until RX (Python regex, multiline) shows up on the module console |
| `--wait STREAM RX` | ... in another stream: `console`, `mcu`, `log`, `trace`, `mcu-trace`, `host`, `net`, `uart0` |
| `--sh CMD` | a command in the module's Linux shell; waits for it to end, fails the session on a nonzero exit |
| `--send TEXT` | type TEXT + CR on the module console (KnOS shell, BL1/BL2 menus; `\xNN` for raw bytes) |
| `--sleep S` | run S seconds |
| `--spi CMDS` | module alone: the SPI test host plays the MCU on J2 (`rd 0x11 rdv 0x25`; `wr REG B0 B1`, `wrv REG HEX`, `req`: `emu/renode/cr870/J2Link.cs`) |
| `--host CMD` | a host command while the boards run as fast as they go (`$RUN` = the run directory, `ZD1DFU_EMU` set with `--mcu`): `--host 'zd1 module power'` drives our MCU firmware. `zd1` is on the PATH in `nix develop` only: run zemu there (`nix develop -c python3 emu/zemu run ...`), else the step fails with exit 127 |

The exit code is 1 if a wait timed out, a `--sh` or `--host` command failed, or the kernel panicked.

**Output**, in `emu/run/NAME/` (gitignored):

| File | |
|---|---|
| `module_uart1.log` | the module console (UART1: the kernel console, the shell, BL1/BL2 menus, KnOS) |
| `module.log` | the module model's own lines: BL1 stand-in, power, watchdog, Warp starts and stops, `CRASH:` (a data/prefetch abort with no handler halts the CPU like a hung chip) |
| `module.trace` | `--trace`: `W4 b5006000 = 00000001  pc=bf01234c`, repeats folded (`... x40`); also GPIO/I2C/DMA/AUDIO event lines |
| `mcu_log.txt`, `mcu_trace.txt` | our MCU firmware's log (USART0), or the stock firmware's console; the MCU's event and MMIO trace (pins, SPI0 bytes `SPI0 xx -> yy`, I2C, ...; ms timestamps) |
| `net.log`, `host.log`, `module_uart0.log` | the user-mode network; host and SPI test host commands; the module's UART0 |
| `nand.bin`, `mcu_flash.bin`, `user_page.bin`, `eeprom.bin` | the state after the session (boot it again with `--nand-image`) |
| `renode.log`, `renode_log.txt` | monitor commands and Renode's own log |

From Python (a longer sequence, a loop): `emu/zemu/rig.py`, `Session` with `start()`, `wait()`, `sh()`, `send()`,
`sleep()`, `spi()`, `host()`, `hold()`, `close()`; `cli.session_config()` builds its configuration.

## Recipes
- **What does a vendor driver do to the hardware?** Trace its block and load it:
  `zemu run -q --trace b5006 --sh 'insmod /lib/modules/2.6.24.7/kernel/.../x.ko'`. The `pc=` values are kernel or
  module addresses: `python3 -I tools/zeppelin/jtag/kallsyms.py --at ADDR` names the kernel's, the modules' are in the
  guest's `/proc/kallsyms` (`--sh 'cat /proc/kallsyms'`). Memory map: `docs/module/memory-map.md`.
- **Stop at a function**: `zemu run --gdb 3333 --keep`, then `emu/tools/gdbtrace.py 127.0.0.1:3333 ADDR@r0+4`
  (logs each hit's registers and memory, no gdb needed) or `gdb-multiarch` with `target remote :3333`.
- **Interrupts**: `--renode "cr870:cpu LogCpuInterrupts true"` (in `renode_log.txt`), `--sh 'cat /proc/interrupts'`.
- **Audio**: `zemu run --wav out.wav --sh 'speaker-test -D hw:0,0 -c 2 -r 44100 -F S32_LE -t sine -f 1000 -l 1'`, then
  `emu/tools/wavstat.py emu/run/os/out.wav --expect 1000`: levels, frequency and dropouts of what came out of AV port 2.
- **The MCU link**: `--mcu` (our firmware; `--host 'zd1 reg read 0x11'`); the bytes on J2
  are the `SPI0` lines of `mcu_trace.txt` (`emu/tools/spiparse.py` checks the framing, `spidiff.py` compares them
  with what the module read). Without the main board: `--spi 'rd 0x11 rdv 0x25'`.
- **Keys and IR into the main board** (they reach zd1-linkd and the apps): `--renode 'zd1:board IR "2000:0x0c"'`
  (ms:RC6 command), `--renode 'zd1:board Pins "1500:PA08=0,1700:PA08=1"'` (ON/OFF), `board Adc "ch:val"`.
  Guest words: `zd1:board Watch "addr:len"` (log changes every ms), `zd1:board Poke "ms:addr:or"`.
- **Vendor firmware**: `zemu run stock --send "os th" --sleep 2` (the KnOS shell); BL1 and BL2:
  `--nand --real-bl1`; BL1's 2 s serial window wants a key held through it: `--keep` and `emu/tools/expect.py` on
  the console socket (`khold:01:3` = BL1's menu, `02` = BL2; it also sends files with XModem). Warp1/Sol from the Mac side: `zemu run --fwd 4557:23
  --host 'python3 -I tools/zeppelin/warp/warprun.py 127.0.0.1:4557 selftest'` (`docs/module/warp.md`).
- **Where host time goes** (a slow boot): `ZEMU_SAMPLE=cr870 ZEMU_SAMPLE_STEP=0.01 zemu run ...`, then
  `emu/tools/prof_samples.py emu/run/NAME/samples.txt` (`ZEMU_SAMPLE_FROM=S` starts later, `ZEMU_SAMPLE_REGS=0,1`).
- **Any other model knob** on the monitor: `core` (module: `Patch`, `TraceOnly`, `DelayLoops`, `TimerWaitPcs`, ...),
  `pm` (MCU trace, WDT), `board`, `usernet`, `spis.testhost`: `--renode "machine:command"`, or a whole `.resc`
  script with `emu/tools/run.py`.

## Tools (`emu/tools/`)

| | |
|---|---|
| `gdbtrace.py` | breakpoint / watchpoint tracer for the GDB stub: each hit's registers and memory words |
| `expect.py` | expect/send on the `--keep` console socket: key holds through boot windows, XModem uploads |
| `wavstat.py` | levels, frequency and dropouts of a recorded WAV |
| `spiparse.py`, `spidiff.py` | the J2 SPI stream from the MCU trace: framing check; MCU side vs module RX FIFO (`--trace b1002e00`) |
| `decode_i2c.py`, `decode_gpio_i2c.py` | bit-banged I2C from the MCU trace's PIN lines / the module trace's GPIO lines |
| `bcdinstall.py` | a `.bcd` into a NAND image the way BL2 writes it (what `zemu run X.bcd` uses) |
| `prof_samples.py` | host-time profile from `ZEMU_SAMPLE` output |
| `run.py` | a Renode `.resc` script through zemu's driver |
| `gen_decoder.py` | regenerates the AVR32 decoder from `renode/avr32/insn.decode` |

## Networking
A user-mode network on the MACB: 192.168.50.0/24, the module gets 192.168.50.101 by DHCP, ARP, ping, DNS refused,
TCP and UDP to the gateway (192.168.50.1) go to the host's loopback, port forwards with `--fwd` / `--keep`. No
bridged networking, so the module is not on the LAN (no AirPlay or Snapcast from a real phone or server).

A receiver against a sender or server on the Mac (a snapserver on 127.0.0.1, `raop_send.py`):
- Start the client late, about 20 s after the steps end with `--keep`: Renode moves from as fast as it can to real
  time then, and a session running across that change stalls or realigns.
- The speaker's volume reaches the receivers only with `--mcu`: without the main board `zd1-linkd` has no speaker
  (it waits for the MCU's hello), so a volume path (Snapcast's included) needs `--mcu`.

## Speed
Our OS alone runs at ~0.5-1x real time while booting, faster when idle (the kernel's idle loop is patched to WFI
here); with the main board linked, a session of 300 virtual seconds took ~100 s. Renode starts in ~2 s (the models
are precompiled into `ZemuModels-<hash>.dll` next to the Renode executable on first use); the MCU firmware build is
cached by source hash. Sessions are deterministic: the same command gives the same instructions and output.
