# The Renode models: both boards

The emulator's machines (what it is for and how to drive it: `../README.md`) as [Renode](https://renode.io) 1.17
platforms, in one Renode process with one virtual clock. `python3 emu/zemu setup` fetches Renode into
`emu/renode/dist`.

Every machine has the same virtual time base and cross-machine events arrive at quantum boundaries, so a session is
**deterministic** (same inputs, same instruction count, same output), the boards are **really linked** (the J2 SPI
link is a time-domain event, not a socket), and powering the module off and on is a reset of its machine.

| Part | What | Source |
|---|---|---|
| AVR32UC CPU | interpreter (Renode has no AVR32 core): AVR32A ISA from the Architecture Manual (32000D) and the UC TRM (32002F), decoder generated from QEMU's decodetree file, 1 ns slots, direct memory arrays, COUNT/COMPARE, events and interrupts | `avr32/Avr32.cs`, `avr32/Avr32Decoder.g.cs` (`../tools/gen_decoder.py`) |
| AT32UC3A1256 | INTC, PM (clocks, WDT, reset causes, GPLP), FLASHC + user page, GPIO, USART, SPI, TWI, TC, ADC, PWM, PDCA, SSC, USBB (EP0, host bridge on TCP: a connection's `R` is a bus reset, the host's requests wait while UDCON.DETACH is set, as it is out of reset; VBUS-transition interrupt, the other endpoints configure but move no data), stores for the rest | `avr32/UC3*.cs`, `platforms/at32uc3a1.repl` |
| Zeppelin main board | DSP, codec, CS2000 (bit-banged), EEPROM, auth chip, keys, IR (RC6), ADC inputs, LED, the module stand-in | `zd1/ZD1Board.cs`, `platforms/zd1*.repl` |
| CR870 module | Renode's ARM926EJ-S (tlib) + our peripherals: VIC, timers (timer 0 = the watchdog), GPIO, the auth chip and a video encoder on bit-banged I2C, AV ports, SuluDMA, NAND with spare areas and the unit's ECC, UARTs, J2 SPI slave, MACB, BL1 stand-in, power and chip reset; SRAM aliased through 0x0-0x3FFFFF, the WILD core's 128 KB SRAM (no core), the pad matrix's 16 copies, live register values (`docs/module/memory-map.md`) | `cr870/CR870.cs`, `cr870/CR870Macb.cs`, `platforms/cr870*.repl` |
| Warp cores | the DM870's two MIPS I cores (Warp1 = the audio engine's controller): debug blocks at 0xE0014000/0xE0015000 (what Nokia's `warp_debugger.ko` and `wsl.ko` use), the cores as measured on the unit (`docs/module/warp.md` section 3: per-core memory maps and bus hangs, no divider, strict load delay, the 4 KB I-cache with stale lines, cycle costs per memory) at 160 MHz in 50 us slices, short slices while a core only polls; Sol = KnOS's reference model (programs run instantly on the start write), which differs from the hardware (section 8) | `cr870/CR870Warp.cs`, `cr870/CR870Sol.cs` |
| J2 link | module SPI slave <-> MCU SPI0/NPCS2, request line PA20, power PA04/PA17, factory-reset strap PB01 -> GPIO 4, module UART1 <-> MCU USART1, as cross-machine events (one quantum latency); the host side mirrors the module's TX FIFO | `cr870/J2Link.cs` |
| Network | a user-mode network on the MACB like QEMU's `-netdev user`: 192.168.50.0/24, DHCP (.101), ARP, ping, DNS refused, TCP to the gateway NATed to the host's loopback, port forwards (`--fwd`) | `cr870/CR870UserNet.cs` |

The models are C# compiled by Renode at start (`include`); zemu concatenates them into one file
(`dist/models-<hash>.cs`, `#line` keeps compiler messages on the original files).

## Time

- One virtual clock for both machines, **serial execution** (one thread runs both): two sessions with the same
  command are bit-for-bit identical, linked ones included (with a thread per machine they were not). The quantum
  (`--quantum-us`; default 200 us with both boards, 1 ms for one) is the J2 link latency and the event granularity
  while a CPU sleeps. With 1 ms the B&W bootloader's register reads over the link fail (it reads the reply before the
  2-quantum round trip). How a session is cut into `RunFor` slices moves IRQs by a few instructions (tlib takes them
  between blocks): the same command repeats exactly, different steps may not.
- `SetAdvanceImmediately`: idle time costs nothing (`--keep` switches to real time). The AVR32 core sleeps in `sleep`.
  KnOS (app and BL2) spins in its idle loop on a tick counter that only interrupts change: `core IdleWfi` (default on)
  patches it to WFI on the OS tick; the same for the Linux kernel's idle loop. The stock app's microsecond busy-wait
  sleeps until its deadline or the next IRQ (`core DelayLoops`). Both leave virtual time and behaviour unchanged.
- MCU 48/66 MHz from the PM's clock tree (1 instruction = 1 cycle); module ARM926 at 240 MIPS.
- A module crash (data/prefetch abort with no handler) halts its CPU like a hung chip and logs `CRASH:` in
  module.log; the watchdog can still reset it.

Speed (M-series Mac, emulated s per host s): our MCU firmware sleeps between main-loop passes, so it runs several
times faster than real time; a busy MCU (the stock firmware) ~0.7x; KnOS idle ~6x, booting ~1x; Linux ~0.4-0.7x
while busy (tlib-bound); both boards linked ~0.6-3x. Per session ~2 s fixed (Renode start; the models are a
precompiled `ZemuModels-<hash>.dll` next to the Renode executable, the images are APFS clones).

## By hand

```
python3 emu/zemu run IMAGE --keep               # the monitor commands zemu sent are in emu/run/NAME/renode.log
python3 emu/tools/run.py script.resc            # a .resc line by line through the same driver
```
Monitor knobs (`zemu run --renode "machine:command"`): `core` (module: `LogFile`, `TraceFile`, `TraceOnly`, `Patch`,
`Stage2Image`, `BootFromNand`, `Power`, `DelayLoops`, `TimerWaitPcs`, ...), `pm` (MCU trace: `TraceFile`,
`TraceMaxLines`, `TraceQuiet`, `TraceLoud`, `WdtResets`, `GPLP0`), `board` (`Key`, `Pins`, `IR`, `Adc`, `Watch`,
`Poke`), `twi.dsp` (`SpdifLock`, `AudioWav`), `spi0.module` (the stand-in: `BootMs`, `PlayMs`), `usernet`
(`Forward`, `LogFile`), `spis.testhost` (module alone: `Run "rd 0x11 rdv 0x38"`), and Renode's own (`cpu
LogCpuInterrupts true`, `cpu DisassembleBlock addr len`, `machine StartGdbServer port`, `sysbus ReadDoubleWord addr`).

## Notes

- The user-mode network only (no bridged networking), with UDP NAT and an RTSP gateway for an AirPlay sender's RTP
  ports (`tools/zeppelin/lab/raop_send.py` with `RAOP_VCLOCK=1` paces itself on the virtual clock).
- Busy polling of the J2 SPI slave by our bare-metal images (module/boot), zd1-linkd and BL2 sleeps until the next
  link event (`spis PollHalt`), so those images do not cost a host second per poll loop.
