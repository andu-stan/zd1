# Documentation

What is true now, with the evidence. Start with `status.md`.

**State and work**
- `status.md`: what is on the unit, what works there, what is emulator-only, what is not done, known problems
- `backlog.md`: open work
- `decisions.md`: why things are the way they are
- `glossary.md`

**Hardware**
- `hardware/zd1-main-board.md`: the MCU, its pins, the audio chain, power, protections, USB, LED/keys/IR
- `hardware/cr870-module.md`: the network module and its 120-pin connector
- `hardware/wiring.md`: what connects to what between the boards

**Main board firmware**
- `mcu/bootloader-dfu.md`: the DFU bootloader, its reset logic, the protocol, the module relay
- `mcu/stock-firmware.md`: what B&W's MCU app does (DSP, codec, amp, sources, modes, stored data)
- our firmware: `../mcu/README.md`

**Board-to-board protocol**
- `protocol/spi-registers.md`: the register protocol, both sides, and the stock MCU's handling
- `protocol/registers.md`: the registers our code uses (generated from `protocol/spi/src/spec.rs`)
- `protocol/sequences.md`: power-up, AirPlay, source switching, BL2, telnet, shutdown

**Host to MCU (USB)**
- `protocol/host.md`: the host link's messages (generated from `protocol/host/src/lib.rs`)
- `web.md`: the web page as installer and configurator: what it does, what it is to become, the supported pairings
  of the two boards, the base layer of the module link, the interlocks

**Network module**
- `module/boot-chain.md`: boot ROM → BL1 → BL2 / app, NAND layout and ECC, image formats, PersParam
- `module/memory-map.md`: memory, peripherals, IRQs, GPIO, DMA, audio out, Wi-Fi
- `module/knos.md`: the stock firmware and its shell
- `module/linux.md`: Linux on the module: how it boots, what works
- `module/jtag.md`: the module over JTAG
- `module/storage.md`: our OS on the NAND: partitions, the system and data partitions
- `module/settings.md`: the speaker's settings (name, Wi-Fi, receivers)
- `module/updater.md`: updating both boards over USB: the flows, the design, what was measured (start here)
- `module/warp.md`: the Warp MIPS coprocessors and the Sol DSP; our Warp0 runtime: `../module/warp0/README.md`

**Streaming**
- `streaming.md`: AirPlay, Snapcast and Spotify Connect on the module: what they cost, how playback stays in sync,
  the Spotify bridge, where the Warps fit; the receivers themselves: `../module/os/README.md`

**Recovery**
- `recovery/factory-reset.md`: back to factory from any brick

The emulator: `../emu/README.md`. Raw reverse-engineering material (decompiler exports, the Wi-Fi/DMA notes, the
Nokia analysis, shell and register dumps): `../re/README.md`.

## Evidence tags

| Tag | Means |
|---|---|
| **[HW]** | measured or seen on our unit (the date where it matters) |
| **[RE]** | read from vendor code (an address where it helps) |
| **[EMU]** | seen only in the emulator: plausible, not confirmed |
| **[INF]** | inferred, untested; the text says how strong |

A claim that has been seen on the unit is [HW] even if it was found by reading code first. Where hardware and the
emulator disagree, the hardware is right and the emulator gets fixed. Older notes (the lab log at git tag
`lab-notebook`) contain conclusions that were later corrected; these docs supersede them.
