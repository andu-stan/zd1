# KnOS: the stock module firmware and its shell

The stock module app (`blobs/module/app_DMP.bcd`, JukeBlox 3 "eDMP", KnOS 3.2 release 9112, built 2014-09) is
BridgeCo's RTOS plus a C++ application: an AirPlay 1 receiver ("AirTunes/103.2", MFi-authenticated, ALAC and L16),
UPnP/DLNA, Bonjour, a GoAhead web UI, wpa_supplicant with WPS, and several dormant music services. Its debug shell
reads memory and NAND, dumps every hardware block, and reaches the SPI register file. Reconstructed from the
decompiled app (`re/ghidra/setup.py module-app`) and the live shell dumps (`re/netmodule/shell-dumps/`), all [HW]
unless tagged.

## Getting a shell
- **On the console**: the shell sits on UART1 (J2 pins 35/37), which is the MCU's USART1: our MCU firmware carries
  it over its host link (`zd1 console`).
- **On telnet**: move the shell to TCP from the MCU side, registers 0x75/0x76/0x77
  (`docs/protocol/spi-registers.md`, section 6): with our MCU firmware `zd1 reg write 0x75 --v 3233000000000000`
  (the port, "23" in 8 ASCII bytes) and `zd1 reg write 0x76 0500`; back to the stock layout with `zd1 reg write
  0x77 --v 3530303031000000` ("50001") and `zd1 reg write 0x76 0400`. The stock MCU patch (`stock-patch/`) does the
  same by re-asserting register 0x76 = 5. The shell has no authentication: keep the module on an isolated network,
  and switch telnet off when done.
- **Wired link from a Mac**: the module DHCPs on its Ethernet port at link-up and falls back to link-local
  169.254.1.1 when no server answers; a USB Ethernet dongle on the Mac (link-local itself, `en8`) reaches it there.
  Bind to the dongle (`ping -b en8`, `curl --interface en8`): with the module on Wi-Fi too, the Mac can route
  169.254.1.1 over Wi-Fi.
- The module binds one network interface at boot and does not fail over: booted on Ethernet, the Wi-Fi device is
  absent (no MIB to read); for Wi-Fi work boot with the Ethernet unplugged. Ethernet and Wi-Fi use the same MAC (MAC0).
- Client: `python3 tools/zeppelin knos [--host IP[:PORT]] 'cmd' ...` (`tools/zeppelin/knos/knos.py`, the library
  every tool here uses). The shell submits lines on CR, so send CRLF, never a lone LF, or its line buffer wedges until
  a CR or a power cycle; the line buffer and XModem state are shared by all connections, so a session starts by
  sending CANs and a CRLF. The listener refuses a reconnect for a few seconds.

## Identity (`sys ver` / `sys versulu`)
KnOS Release 9112, tag `JB3_0F_edmp_32MB-3_8_0-9112_ER`, built 2014-09-15; app `JB21.0-Ref`, `DM870_HAL`. Components:
CnE at NAND 0x40080, image at 0x240080, bootloader at 0xA0080 (2010-09-27). Chip blocks all rev 1 (2006): arm926 rev 5,
uart0/1, spi0, nfh (NAND), dma, av ptu/common/av0/av2/av3/av4. Matches `docs/module/memory-map.md`.

## Command surface (`help`)

| Command | What |
|---|---|
| `rd <addr> <count> [b\|h]` / `wr` | raw memory read/write (`rd -c` / `wr -c` bypass the MMU check and can fault the module) |
| `sys memdump` / `sys memscan` | memory dump / scan |
| `sys clock / irq / sdram / macb / dma / av / swmtx / versulu` | per-block introspection |
| `sys wlan ctrl \| mib \| pci \| test` | the Wi-Fi driver surface (`mib` = the WILD MAC config; `re/netmodule/WIFI_DRIVER.md`) |
| `sys spi open/rd/rw/wr/stat/slave/reg`, `spilog` | the MCU-link transport |
| `reg print [id] \| add <id> <name> <size> \| write <id> …` | the SPI host-register file (ids in decimal; `add` makes a RAM-only register served to the MCU) |
| `persparam rd/rds/wr/wrb/check` | the PersParam store (`rd HWID` = 0xBD) |
| `get` / `set <cne-path>` | the CnE config tree (`cne/...`); `set` writes, so avoid unless meant |
| `fburn listFlashAddr / rd / bin / ebin / img` | NAND read/write (FlashId 0 = NAND, XModem; `docs/module/boot-chain.md`) |
| `sys sol ...` | the Sol DSP from the ARM (hung the module twice: `docs/module/warp.md`) |
| `filetransfer`, `apple` / `ipodauth`, `wpa`, `netcfg`, `os …` | file transfer, the MFi auth chip, wpa_supplicant CLI, network config, the OS introspection tree |

## OS
Threads: coreThread, PowerManagerThread, Inet main, WlanThread, WpaSupplicant, StreamDeviceMgrImpl, USBDrvRxThread,
USBHostController, UPnP/SSDP, **SPI Slave**, PSMThread, CardeaThread. Idle ~88 %. `os` has
blk/byte/cne/evt/flg/free/load/map/mchk/mlc/mseg/mtype/q/sem/th/timer/timestamp/running/standby/suspend/sleep/
uartcfg/wp. The config DB behind the SPI register map is the CnE tree (`ls`: `static` + `cne`, ~60 nodes).

## Reading NAND and memory
- Memory: `rd <addr> <n>` or `sys memdump` (SDRAM, SRAM, mapped MMIO). The running app is at SDRAM 0x60008000.
- NAND: `fburn rd <id> <addr> <len>` streams XModem-CRC (CRC16-CCITT, 128/1024-byte packets). `zeppelin fburn rd`
  sends CAN first to cancel any stuck transfer, waits for "Start xmodem now!", then receives. `zeppelin fburn wr`
  drives `fburn ebin`, but over telnet the shell never sees the confirmation (`docs/module/boot-chain.md`, "Writing
  NAND from the stock app"). An aborted `fburn rd`
  leaves the sender spewing EOT into the shared shell, so always CAN-flush first. `blobs/module/bl2.bin` and the live
  PersParam/CnE blocks were read this way.
- `re/netmodule/inject/` runs a small Thumb payload in the app's RAM through the shell (its `README.md`).

## Operational notes
- The shell has full memory, NAND (`fburn`), Wi-Fi and MFi (`ipodauth`) access with no authentication.
- `rd` of some uninitialised regions, or `rd -c` of an unmapped address, faults the module (the watchdog reboots it
  in ~15 s; harmless unless it happens during a NAND write). Keep probes read-only and avoid blind addresses.
- Never read or record the Wi-Fi secrets: `reg print` incidentally shows registers 95-102 (WEP keys, cipher, **WPA
  passphrase**, SSID); `cneu --raw` would show them too. The redacted path is `python3 tools/zeppelin cneu`.
- The stock app crashes in its Wi-Fi code when made to use Wi-Fi without a WILD core [EMU only: a NULL driver
  object].
