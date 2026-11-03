# The module over JTAG

The DM870's JTAG is on J2 pins 22/24/26/28 (TMS/TCK/TDI/TDO, no TRST on the connector; `docs/hardware/cr870-module.md`),
reachable on the B&W carrier board's four test pads. A SEGGER J-Link with VTref on a 3.3 V point of the carrier and
OpenOCD 0.12 (`nix build nixpkgs#openocd`) see one TAP [HW 2026-10-23]:

    IDCODE 0x15900f0f, IR length 4, EmbeddedICE version 6, 2 breakpoint/watchpoint units -> target arm926ejs

`tools/zeppelin/jtag/dm870.cfg` is the OpenOCD config (J-Link, JTAG, 1 MHz works). The chain reads a constant TDO
(all ones or all zeroes) until the four leads sit on the right pads; the pad order on the carrier is not the obvious
one.

## What it gives
- **Halt and inspect the running module without any console**: `halt`, `reg`, `mdw`. With the MMU on, kernel
  virtual addresses read directly, physical ones with `mdw phys`. Reading an unmapped virtual address makes the
  core take a data abort (OpenOCD recovers, the kernel may not): use `phys`. Every halt stops the kernel's clock, so
  log timestamps lag.
- **Always `resume` in the same OpenOCD session.** On the ARM926 OpenOCD reads and writes memory (`mdw`/`mww`,
  `phys` too) by running the core with its own registers and puts them back only on `resume`. A session that ends
  with the core halted loses them: the next session takes the clobbered registers as the real ones, and its `resume`
  crashed the kernel ("Attempted to kill the idle task", r1/r2 = the words just written) [HW 2026-10-25]. Wrap the
  commands so an error still resumes: `-c 'halt 2000' -c 'catch { ... }' -c resume -c shutdown`.
- **Don't halt during boot.** A halt in the boot ROM (OpenOCD reads its cpsr as 0) and a resume, or hardware
  breakpoints left armed across a chip reset (`reboot`), sent the core into BL1's undefined-instruction stub
  (0x00403624, `b .`) [HW 2026-10-25]; only a module power cycle (`mod off`, `mod power`) got it out. What the chip
  holds there is its reset state: GPIO A 0x000FFFFF, B 0, the pad matrix 0 but for +0x20 = 0x900 and +0x28 = 1.
- **The kernel log from RAM**: `python3 -I tools/zeppelin/jtag/kmsg.py [--openocd PATH] --pc` halts the core for a
  second, dumps the 64 KB ring buffer through the kernel's mapping and prints it (`--pc`: where the core was). The
  buffer's address, length and write index come from the literal pool of `log_buf_copy` (0xc02b7c4c / 0xc02b7c48 /
  0xc02e4134; the kernel's kallsyms have no static variables). Our init scripts and `zd1-linkd` mirror
  their messages into `/dev/kmsg`, so this is the whole boot story.
- **BL2 without the MCU**: write the mailbox words `{0xDEADBEEF, 0, 0x21524110}` to 0xB1005000, then a reset pulse
  on PA17 from the MCU [HW]. With `zd1 poke -w 0xFFFF1058 00020000`, then `poke -w 0xFFFF1054 00020000` (GPIO
  port A's OVRC / OVRS) is the untried way; `mod hold` power-cycles, which clears the mailbox. Not a power cycle: that clears the mailbox.
- A full RAM dump (`dump_image file 0xc0000000 0x1f00000`) runs at ~10-20 KB/s through this J-Link: 30-50 min.

Power the module for a session with `zd1 module hold`: power, 1.2 s, reset release, and nothing else (no SPI, no
time-out; `mod off` ends it). Our firmware's own bring-up (`mod start`, at every MCU boot) powers the module off after
180 s without READY, and power-cycles a system slot that isn't ready in 90 s: hold the module for anything longer.

## The register-diff method
Dump the peripheral window (`mdw phys`: 0xE0000000-0xE001FFFF, 0xB1000000-0xB1007FFF, the GPIO block) under Linux
and again under BL2 (mailbox + reset pulse), diff, replay the differences into the running Linux one at a time and
read the effect at once (the GPIO level register, the MAC's NSR MDIO bit, an MDIO read by hand through MAN
0xE0017034, the kernel log's eth0 retry). This found the pad switch matrix, the GPIO semantics, the PHY's
PLL dependence and the request-line rule (`docs/module/memory-map.md`). The dumps: `re/netmodule/jtag/`.
