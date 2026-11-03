# Glossary

| Term | Meaning |
|---|---|
| ZD1 | B&W's codename for the Zeppelin Air (30-pin dock version); also this project's name (zd1) and its MCU firmware's |
| BCO, BridgeCo | the module's maker (later SMSC); "BCO" in the MCU firmware = the network module |
| CR870 | the plug-in network module; CR870-2C is the Zeppelin's variant |
| carrier board | B&W's board the CR870 plugs into; a 14-pin link to the MCU board, a cable to the back board |
| DM870, Sulu | the module's SoC (ARM926EJ-S); "Sulu" is BridgeCo's family name, also in Nokia's Linux (`mach-sulu`) |
| JukeBlox (JB) | BridgeCo's networked-audio platform/SDK; build 9112 = 0x2398 on this unit |
| KnOS | BridgeCo's RTOS in the stock module firmware (3.2) |
| WILD | the DM870's on-chip Wi-Fi core (Eagle MAC, its own ARM7) |
| Warp, Sol | the DM870's two 160 MHz MIPS cores (Warp0 secure/DRM, Warp1 audio) and the DSP datapath Warp1 drives (`docs/module/warp.md`) |
| SuluDMA | the DM870's DMA engine (packet and block moves) |
| AV port 2 | the DM870 audio output (I2S to the main board's DSP) |
| pad switch matrix | DM870 block at 0xE0013000 that connects pads to peripherals; the Nokia kernel never sets it |
| BL1 / BL2 | module first-stage bootloader (NAND block 0, encrypted) / second-stage updater (NAND 0xA0000) |
| stage 1, preloader | the 8 KB SRAM loader inside every app/BL2 image |
| boot stub, linuxboot | our bare-metal stage in front of the Linux kernel (`module/boot`) |
| `.bcd`, bCoD | module firmware container (app + factory CnE + resources) |
| MetaInfo | the 0x80-byte partition header in NAND |
| CnE | the module's configuration database (factory part at 0x40000, user part at 0x60000) |
| PersParam | the module's persistent parameters block (MAC, HWID, CnE pointers) at NAND 0x20000 |
| BLDb | the PersParam key that makes BL1 start BL2 at every boot |
| HWID | hardware id checked by BL2 (0xBD for this product) |
| HCR / DCR / DRC | host control (MCU → module commands, reg 0x28), device control (module → MCU, 0x2A), remote-display commands (0x48) |
| NAP | the module's audio player state (Searching, Connecting, Buffering, Playing) |
| RAOP | AirPlay 1's audio protocol (RTSP on TCP 1024 on the stock module); DACP = its remote-control side |
| MFi | Apple's accessory authentication (the auth coprocessor chips) |
| iAP | Apple's iPod accessory protocol (dock) |
| DFU, ISP | USB firmware update mode of the MCU bootloader; ISP_FORCE = the fuse bit that forces it |
| ISP word | the user-page word (0x929E0870) the bootloader validates |
| `module esc`, the console | `zd1 module esc` (`Module ESC`): BL1's serial menu into BL2 over the module console, the MCU driving it. `zd1 console`: a terminal on the module's console over the host link |
| host link | our MCU firmware's typed requests and log over USB (a vendor interface on EP0): `zd1`, `docs/protocol/host.md` |
| system slot, rescue, data partition | our module OS on the NAND: two slots (`system-a`, `system-b`), each a whole root the kernel mounts read-only; the rescue, an initramfs in the app partition that starts when no slot gets ready; `/data` (settings, the AirPlay identity), all JFFS2: `docs/module/storage.md` |
| zd1, cr870 | the two emulated machines (Renode, `emu/README.md`) |
| zemu | the emulator driver (`emu/zemu`): `zemu run IMAGE` boots and drives an image |
| zd1-link, zd1-linkd | our module-side link code (`module/link`, the protocol library); zd1-linkd = the link under Linux (`module/os/apps/zd1-linkd`) |
| native dialect | the registers our MCU firmware and our OS use between them (0xC0-0xC6, 0xD2-0xD5) instead of the stock app's DRC codes, volume and play-state XML: `docs/protocol/sequences.md` |
| Warp0 runtime | our firmware on Warp0 (crypto, AAC, Vorbis, FLAC) and its ARM client: `module/warp0/` |
| unit | one physical speaker; per-unit data lives in `units/` |
