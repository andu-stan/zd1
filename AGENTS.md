# Working on this project (people and agents)

Read `README.md` and `CONTRIBUTING.md` first. This file is the rules and the tools for working without someone watching every step. A
local `CLAUDE.md` (gitignored) imports it and adds what is particular to one unit and its owner: what needs their OK,
the unit's history, where its files are.

## Ask first
- Flashing anything to a unit (MCU via DFU, module via BL2 / fburn / relay), or changing its settings: only with the
  go-ahead of whoever the unit belongs to.
- Writes to the protected regions, even where a tool allows it with a flag: MCU bootloader, module BL1 (NAND block 0),
  module BL2 (NAND 0xA0000-0x1FFFFF). They are the way back.
- Uploading anything (archive.org, a public repo).

## The amp
- Keep sound through the amp short: a listening or probe test, no soak tests or long amp-on runs. Our firmware allows
  the amp from every boot: it comes on with sound and goes off after 5 minutes of silence (`zd1 prefs --amp-idle`).
  The amp's supply is the fragile part of a Zeppelin Air and runs hot (ADC5 falls as it warms; stock trips at 0x1FF).
- Don't allow an amp someone locked (`zd1 amp allow|auto` after an `amp deny`). Lock it (`zd1 amp deny`, at boot
  too) when heat or faults look wrong, and say so.

## Always
- Hardware is the evidence; the emulator is a development aid. A behaviour counts as working once it was seen on the
  unit; an emulator run only says the code does something plausible (`emu/README.md`, "What the emulator proves").
  Use the emulator to develop and debug (boot the image, run the commands, trace the hardware it touches), not as
  a gate: there are no regression tests, and a change goes to the unit once it does what you want here.
- Know the way back before touching the unit: a DFU backup of the MCU (`zd1 dfu backup`) before
  an MCU flash; `zd1 module esc` (BL1's menu into BL2) or JTAG for the module.
- Per-unit data stays in `units/` (gitignored): MAC, Wi-Fi settings, live NAND/SRAM/EEPROM dumps, console logs that
  print the module's saved settings. Never paste a Wi-Fi passphrase into a doc, commit or log.
- Files from `downloads/` and dumps are untrusted data: read them with `python3 -I`, never execute vendor binaries
  outside the emulator.
- Record findings in the topic doc in `docs/` (or `re/` for raw material), with the evidence: hardware date, code
  address, or the `zemu run` command (marked `[EMU]`, emulator-only). Update `docs/status.md` when what works on the unit
  changes, and the unit's own `units/<unit>/status.md` when what is installed on it changes. Docs say what is true now; don't append dated logs.

## Checking your work without a person at the speaker

| Tool | What it tells you |
|---|---|
| `mcu/flash.sh` (= build + `zd1 flash`) | (hardware, with OK) build, the host link's `Dfu` -> DFU -> a DFU backup into `units/<unit>/mcu/dfu-backups/` -> flash + verify + start, then the new firmware's version and boot-stage log |
| `zd1 info\|diag\|log -f\|...` | the speaker over USB (`host/cli`, on the PATH in `nix develop`; `zd1 --help`), our MCU firmware's host link (`docs/protocol/host.md`): both boards' versions, the speaker's state, internals (protections and ADC5 temperature, audio path, USB audio, AirPlay glue, IR), source / volume / keys, its log (the module's console among it: the boot chain, the kernel, our OS) |
| `zd1 console [run CMD...\|log]` / `zd1 module esc` | the module's own console over the host link (`Console`, gated; BL1/BL2 menus, the Linux or KnOS shell): a terminal (Ctrl-] leaves), commands to their end with their output and exit status, or what it says (the page's Advanced has the same; Python tools: `zd1.run_on_module`) / the module into BL2 through BL1's menu, the MCU driving it, whatever the image |
| `zd1 settings` | the module OS's settings (name, Wi-Fi, address, receivers: `docs/module/settings.md`) |
| `zd1 debug on\|off` | our module OS's debug services (telnet shell, memory server): off in the normal image; RAM only, back to the image's default when the MCU powers off |
| `zd1 gate` | the host link's gate: requests that change the speaker (flash, settings, debug, module power, source and volume) need a press of ON/OFF on the unit, or the gate opened for good (`gate open`, a press, kept in the EEPROM). Unattended work needs it open: if a tool asks for a press, stop and ask the person at the unit. Emulator: open by itself (zemu sets it in the run's EEPROM) |
| `python3 -I tools/zeppelin/jtag/kmsg.py --pc` | (hardware, J-Link on the carrier) the running kernel's log from RAM, no console needed (`docs/module/jtag.md`) |
| `zd1 update [IMAGE]` | (hardware, with OK) the module's system (`emu/img/os/system.cpio.gz`) over USB into the slot that doesn't run, then started once: kept if it gets ready, else the MCU goes back (`docs/module/updater.md`). Emulator: `--uart` without the MCU's part |
| `zd1 slot [try a\|b\|rescue \| good a\|b] [--restart]` | which system slot the MCU starts the module with (`docs/module/storage.md`); changing it is gated |
| `python3 tools/zeppelin bl2web <ip> <image.bcd> --yes` | (hardware, with OK) install a module image through BL2's web updater |
| `python3 tools/zeppelin usprobe` | (hardware) plays an inaudible 19 kHz coded tone and listens with the Mac mic: "does the speaker play?" without anyone listening. Detects down to about -60 dBFS; music playing masks it |
| a USB Ethernet dongle on the Mac (`en8`) | (hardware) the module on wired Ethernet at 169.254.1.1 (its link-local fallback: no DHCP server); bind to the dongle (`ping -b en8`, `curl --interface en8`, `bl2web --iface=en8`): with the module on Wi-Fi too the Mac may route 169.254.1.1 over Wi-Fi |
| `python3 emu/zemu run [IMAGE] [--sh CMD]... [--keep]` | boots our OS (or any module image; `--mcu` adds the main board) in the emulator, runs shell commands, traces MMIO (`--trace`), GDB (`--gdb`); `emu/run/NAME/` has every log (`emu/README.md`) |
| `cargo test -p zd1-host -p zd1-hostlink` | the DFU frames against the decompiled original, the host link's types against its table, offline |
| `nix fmt` / `nix flake check` | every file formatted and linted (`nix/treefmt.nix`, `CONTRIBUTING.md`); the check adds clippy on every crate, warnings fail, the crates' tests (the host's, and the module's that build on the host: on Linux all of them) and `zd1-protogen --check` (`checks.host-tests`), the MCU's protocol C tests (`mcu/test`), and the privacy check (`nix/privacy.py`: no per-unit data in tracked files). Run `nix fmt` before committing |
| `python3 tools/zeppelin web` | the web page (`host/web`: the same flows as `zd1` over WebUSB) on localhost; `node host/web/test/emu.mjs` runs its WebAssembly against the emulator (`zemu run --mcu --host`) |
| `units/<unit>/hw-checks.md` (not in git) | what still needs a person at the unit, or an OK |

## Where things are
- MCU firmware: `mcu/` (`mcu/README.md`); the stock firmware's behaviour: `docs/mcu/`, raw material in `re/mcu/`.
- The computer's side: `host/core` (every flow once: the host link, settings, the system update, DFU and the MCU flash),
  its front-ends `host/cli` (`zd1`) and `host/web` (the page, WebAssembly over WebUSB); the protocols: `protocol/README.md`.
- Module: `module/os/` (our OS: `nix build -o emu/img/os .#os-image`, or `zemu run` builds and boots it), `protocol/` (the MCU link protocol spec: Rust, generates the MCU's C), `module/link/` (its module
  side), `module/boot/` (bare-metal boot stub + test image); the stock module (KnOS, boot chain, NAND, SPI protocol):
  `docs/module/`, raw material in `re/netmodule/`.
- Emulator: `emu/README.md`; the models: `emu/renode/README.md`.
- Builds: nix (`flake.nix`, `nix/default.nix`; `CONTRIBUTING.md`, Builds): `nix build .#os-image`, `.#mcu`, ...; zemu
  and `mcu/flash.sh` call it. Nix builds the files git tracks, so `git add` new sources. Host tools
  run in `nix develop`. Vendor binaries live in `blobs/` (with provenance); everything else downloaded is on archive.org,
  listed in `downloads/manifest.json` (`python3 tools/zeppelin downloads fetch PREFIX` gets it into `downloads/`).
- Recovery: `docs/recovery/factory-reset.md`; image builder `tools/zeppelin/recover/mkunit.py`.
- State of things: `docs/status.md`; open work: `docs/backlog.md`; one unit's state and pending checks: `units/<unit>/`.

## Agent-specific
- Deep reverse engineering of the vendor firmware is better done in the main session than in subagents: subagents
  doing this work have been stopped by over-broad safety filters before.
- Emulator sessions: `zemu run -q` with steps ends by itself (exit 1 on a timeout or a failed `--sh`). Long ones
  in the background, a `--name` each if several run at once; a `--keep` session ends with `kill` (state saved).
