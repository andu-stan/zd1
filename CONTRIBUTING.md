# Working on zd1

How the repository is laid out, how everything is built, and the rules for working on real hardware. What the
project is and how to install it: `README.md`. The documentation, with the evidence for every claim: `docs/README.md`
(start with `docs/status.md`). The tools for checking work without someone watching the speaker, and the rules for
agents: `AGENTS.md`.

## Ground rules
- **The way back stays.** The MCU bootloader (0x80000000-0x80003FFF), the module's BL1 (NAND block 0) and BL2
  (0xA0000-0x1FFFFF) are how any unit recovers. The tools refuse to write them (BL2 only with an explicit flag);
  don't. Know the way back before you touch a unit: a DFU backup of the MCU (`zd1 dfu backup`, `mcu/flash.sh` takes
  one), and `zd1 module esc` or JTAG for the module.
- **Short amp tests.** The amp's power supply is the fragile part of a Zeppelin Air. Keep sound through the amp to
  a listening or probe test: no soak tests. `zd1 amp deny` locks the amp (at boot too) when heat or faults look
  wrong.
- **Per-unit data stays out of git**: MACs, Wi-Fi settings, NAND/SRAM/EEPROM dumps, logs that print the module's
  saved settings. They go in `units/<unit>/` (gitignored). `nix flake check` fails on a tracked file with a MAC
  other than the test MAC `EC:66:D1:00:00:42` (or a `02:…` one), a BSSID, a unit directory's path, a home LAN
  address or a home directory (`nix/privacy.py`); examples use those, `192.0.2.x` and `units/<unit>`.
- **Vendor files are untrusted data**: `downloads/` and dumps are parsed (`python3 -I`), never run outside the
  emulator.
- **The unit is the evidence.** A behaviour works once it was seen on a speaker; the emulator is for developing and
  debugging, and an emulator-only result is marked `[EMU]` (`emu/README.md`, "What the emulator proves"; the other
  tags: `docs/README.md`). There is no regression suite: a change goes to the unit once it does what you want in the
  emulator.
- **Docs say what is true now**, with the evidence: the topic doc in `docs/` gets the finding, `docs/status.md` what
  works on the unit. No dated logs, no history.

## Layout

| Path | What |
|---|---|
| `mcu/` | our MCU firmware (ASF; USB composite: audio, HID, the host link with the module's console): `mcu/README.md` |
| `module/os/` | **the module OS**: our userspace (musl, busybox, Rust apps as one executable, our forks of the receiver libraries: `module/os/forks.md`) on the Nokia kernel; toolchain, build, rootfs |
| `protocol/` | the MCU's two links as Rust specs: the SPI register protocol to the module, the host protocol over USB; they generate the MCU's C, its tests and the protocol references (`protocol/README.md`) |
| `module/link/` | the module side of the MCU link (`no_std` Rust library) |
| `module/boot/` | bare-metal module images: the Linux boot stub, a minimal test image |
| `module/warp0/`, `module/codecs/` | the Warp0 compute runtime (the DM870's MIPS core running our decoders), the AAC, Vorbis and FLAC decoders |
| `stock-patch/` | patch the stock MCU firmware: the module's shell on telnet |
| `emu/` | the emulator, a development and RE aid: the module and the main board in Renode (`emu/renode`), `zemu run` boots and drives an image (`emu/README.md`) |
| `host/` | the computer's side of the speaker over USB, once (`host/core`): `zd1` (`host/cli`: state, controls, settings, updates of both boards, DFU) and the web page (`host/web`: the same over WebUSB; `docs/web.md`) |
| `tools/` | Python tools behind one command, `python3 tools/zeppelin --help`: the web page's server (`web`), KnOS shell/NAND, image formats, recovery, JTAG, Warp probes, lab probes |
| `blobs/` | the vendor binaries everything needs (bootloaders, stock images, Nokia's kernel and drivers), with provenance (`blobs/README.md`) |
| `re/` | reverse-engineering material: notes, raw dumps; the decompiler exports are regenerated (`re/README.md`) |
| `docs/` | the documentation (`docs/README.md`) |
| `AGENTS.md` | the tools and rules for working unattended (a local `CLAUDE.md` imports it) |
| `flake.nix`, `nix/`, `Cargo.toml` | the builds (every firmware piece and its toolchain, pinned), the dev shells; the Rust workspace |

## Builds
```
nix develop                   # Python with the tool deps, LLVM, host Rust, OpenOCD, zd1 from the checkout
python3 tools/zeppelin --help
python3 emu/zemu setup        # fetches Renode (pinned) into emu/renode/dist
python3 emu/zemu run --keep   # our module OS in the emulator (nix builds it); nc 127.0.0.1 4555 = its shell
```
Nix builds everything (`flake.nix`, `nix/default.nix`), every toolchain pinned by `flake.lock`, each piece rebuilt when
its own inputs change. The module side cross-compiles with nixpkgs' LLVM toolchain (clang, lld, compiler-rt, static
musl; `nix/armv5-compiler-rt.nix` fixes compiler-rt for the ARMv5TE target) and Rust through crane, and the module's
image and every part of it (the module rows below, `boot` and `debug-tools` included) are built on Linux: on a Mac,
`.#os-image` is the aarch64-linux one, so the same source gives the same bytes (macOS builds in a new directory every
time, and the binaries follow it; `flake.nix`). The MCU needs Linux too: Microchip's AVR32 toolchain is an x86_64
Linux binary (LLVM has no AVR32). On a Mac nix hands both to a Linux builder (nix-darwin's `nix.linux-builder` or a
remote one, aarch64 and x86_64). Nix sees the files git tracks: `git add` a new source file before building.

CI pushes what it builds (x86_64-linux) to a public cache: add it to your nix configuration to skip those builds
(`extra-substituters = https://pawlowskialex-zd1.cachix.org`, `extra-trusted-public-keys =
pawlowskialex-zd1.cachix.org-1:fAJlCZ696sXLcIrYO9EZWCVLkw0T90T6gsjUoR6TOv8=`, or `cachix use pawlowskialex-zd1`).

| `nix build .#…` | What |
|---|---|
| `os-image` | the module OS images: `os.bcd`, `os-debug.bcd`, the system images, the emulator's payloads and NAND (`module/os/README.md`) |
| `mcu`, `mcu-emu` | our MCU firmware (`mcu/Makefile`): `zd1.{elf,hex,bin,map}` with the commit stamped in (`mcu/stamp.py`; `mcu-raw` and `mcu-emu-raw` unstamped), `mcu/flash.sh` flashes it; the emulator's build (log and console on USART0 too) |
| `avr32-toolchain` | Microchip's AVR32 GNU toolchain the MCU builds with (below) |
| `test-unit` | the emulator's factory-fresh unit (MCU flash, EEPROM, NAND) from `blobs/` |
| `module-os-parts`, `busybox` `busybox-rescue` `alsa-lib` `alsa-utils` `wpa_supplicant` `apps` `apps-core` `apps-debug` `boot` `warp0-fw` | what the OS image is made of, together and one by one (`-rescue`, `-core`: the rescue's; `-debug`: the debug system's) |
| `debug-tools` | tools to copy to the module: the probes in `module/os/debug` |
| `telnet-hook` | the hook `stock-patch/patch.py` splices into the stock MCU firmware (`stock-patch/build.sh` refreshes the committed one) |
| `zd1` `web` | the speaker over USB from a terminal (`nix run .#zd1 -- --help`; `nix develop` has `zd1` from the checkout), the web page as a directory (`python3 tools/zeppelin web` serves it) |
| `airplay2-rs` `llvm` | host tools: the AirPlay test sender (`nix run .#airplay2-rs`), the LLVM the Warp and bare-metal tools use |

A variant through override: `boot.override { tag = "v2"; }` (`module/os/assemble.py mini v2`). All the Rust is one cargo
workspace at the top (`Cargo.toml`, one toolchain: `rust-toolchain.toml`, a nightly): the host side (protocol, link
library, codec checks, tools; `cargo test` runs them), the module's userspace and its boot stub, and the libraries they
share, each named once in `[workspace.dependencies]`. The module's builds are `cargo module -p zd1` (the
programs in one executable: `zd1 PROGRAM` runs one) and `cargo boot` (`.cargo/config.toml`), in `nix develop .#cross` (the linker and C
compiler for the module, alsa-lib and the Warp0 images) and in nix the same way. `nix develop .#re` has Ghidra.

The AVR32 GNU toolchain (Microchip, an x86_64 Linux binary that Microchip no longer serves at a stable URL) comes from
our copy on archive.org (`avr32-gnu-toolchain-3.4.2-linux-x86_64`, `nix/mcu/avr32-toolchain.nix`).

The loop on a unit: `mcu/flash.sh` (build, the host link's Dfu -> DFU -> a backup -> flash + verify + start),
`nix build -o emu/img/os .#os-image` then `zd1 update` for the module's system, `zd1 info|diag|log -f` and
`zd1 console` to see what it does (`AGENTS.md` has the whole table). Commit before a flash, so the version stamps
name a commit.

## Formatting and checks
`nix fmt` formats every language in the repo and runs the linters that work file by file (`nix/treefmt.nix`: rustfmt,
ruff, clang-format + cppcheck, nixfmt + statix + deadnix, shfmt + shellcheck, biome for the web page, taplo,
google-java-format, csharpier), fixing what they can and reporting the rest; their settings are at the top
(`rustfmt.toml`, `ruff.toml`, `.clang-format`, `biome.json`, `taplo.toml`). The vendored ASF, the decompiler's C in
`re/` and the generators' output are left alone. Run it before committing.

`nix flake check` fails on a file `nix fmt` would change, a linter's finding, or a clippy warning in any crate: the
host's, the web page's, the boot stub's and the module's (`cargo clippy` as each is built). It runs the crates' tests
(the host's, and the module's that build on the host: on Linux all of them) and fails on a stale generated file
(`zd1-protogen --check`), runs the MCU's protocol C against the Rust's vectors (`mcu/test`, on the host's compiler),
and runs the privacy check above. A finding that is wrong for a line gets an inline suppression with its reason
(`// cppcheck-suppress ID ; why`, `# noqa: RULE`, `#[allow(clippy::…)]`).

## What's not in git
- `downloads/`: B&W installers and firmware, other JukeBlox devices' firmware, FCC exhibits and datasheets. They are on
  archive.org; `downloads/manifest.json` (tracked) lists every file with its sha256 and source, and what was left out
  and why. `python3 tools/zeppelin downloads fetch [PREFIX]` downloads them into `downloads/<item>/<path>`
- `units/`: per-unit data, and per unit what is installed on it (`status.md`) and the checks waiting for a person
  at it (`hw-checks.md`)
- `CLAUDE.md`: the local agent instructions, `AGENTS.md` plus one unit's and its owner's rules
- `re/gen/`: the Ghidra analyses of the vendor images and their text exports (`re/ghidra/setup.py`)
- `re/netmodule/extracted/`: carved from the vendor images (`python3 tools/zeppelin bcd ...` regenerates)
- `emu/img/` (nix links, caches; `emu/img/roots/`: what `tools/zeppelin/gcroots.sh` keeps through a `nix store gc`, the
  current images and the dev shells), `emu/run/` (emulator runs), `target/` (the workspace's builds)

## Upstream
The receivers build on our forks of shairplay-rust, librespot and libmdns (`module/os/forks.md`: what each changes,
pinned in the top `Cargo.toml`); the changes there are meant to go upstream, one PR each.
