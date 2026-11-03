# MCU ↔ module sequences

The register traffic for each thing the two boards do together. Registers and formats: `spi-registers.md`. Each
section says whether it has been done on the unit [HW] or only read from code [RE] / run in the emulator [EMU].

## Power-up to ready [HW]
1. PA17 low, PA04 low (module off). Attach: **PA04 high**, wait **1200 ms**.
2. **PA17 high**, SPI0 on (mode 3), wait **200 ms**, then 1 s.
3. Wait for PA20 high, read **0x11**: `20 00` = booting, keep polling while PA20 is high (on a read timeout retry after
   300 ms); `21 00` = ready; `20 00` = booting (also our boot stub); `23 00` = BL2. Stock app: ready ~15 s after PA17.
4. While PA20 is high: read **0x0C** → register number → V-read that register → dispatch; repeat until PA20 drops.
   After ready the stock app queues ~24-32 notifications (0x47 DRC XML views, 0x32 "21", 0x38 IP, 0x39, 0x3A, 0x40,
   0x56, 0xD1). Then re-read 0x11 every 3 s as a heartbeat.
5. Supervision: stock retries a read that fails for 60 s, 30 times (~31 min), then raises system error 5/10/11 and
   powers the module off [RE]. Our firmware: the bring-up at boot waits 180 s for ready, then powers the module off;
   a system slot our boot stub was told to start gets 90 s, then the module is power-cycled onto another slot
   (`docs/module/storage.md`, The boot); `Module POWER` waits up to its timeout (120 s by default).
6. Stock setup task after ready [RE]: read 0x25/0x26/0x27 (the "9112" gate on 0x25), 0x45 `R pssCne/CneId`, `W
   SystemHost/HostVersion = 2.04.05`, read 0x32; then the network-settings sync into the MCU user page.

## AirPlay starts (module → MCU) [HW, our MCU firmware + the stock module]
1. A sender streams to the module (RAOP on TCP 1024); the module's player goes Connecting → Buffering → Playing.
2. The module pushes **0x47** `<update id="status">…<icon id="play">pl</icon>…` (needs a DACP-style session: a bare
   RAOP stream shows "Now Playing" but play stays empty).
3. MCU: auto-switch to Wi-Fi (stock: only while DRC state machine 2 is idle), route the DSP to the module input
   (E080 = 0002, E002 = 0908, 0x14 = 0), amp sequence, **DRC 0xDF** (ack 0xE3 = 00) and **0xE2** (ack 0xE3 = 05).
4. Only after 0xE2 does the module send real samples on I2S (before: zeros).
5. Volume: module → MCU `03 03 x vv` on 0x40/0xE4 (vv 0 = loudest .. 0xF0, > 0xF0 = mute); MCU → module W 0x40
   `03 03 01 vv` on local changes. Handshake: `03 01 x 00` → reply `03 01 01 02`, `03 01 x 03` → `03 01 01 04`.
6. Stop: 0x47 `pa` (paused) or `em` (empty); the source stays on Wi-Fi.

## Source switching [RE]
- Entering Wi-Fi (and at boot, as "startup resume"): DRC 0xDF then 0xE2. Leaving (other source, standby, iPod):
  0xDE (ack 01) then 0xE1 (ack 04). Each: wait 2 s, send, wait ≤ 3 s for the ack, resend while still pending.
- Transport keys while on Wi-Fi: DRC 0xB0 next, 0xB1 previous, 0xB3 play/pause (no ack).

## Into BL2 (module recovery)
- **HCR** [HW]: W 0x28 = `21 00` while the image is ready (the stock app, or our `zd1-linkd`). The app writes the BL2
  mailbox and resets itself through the watchdog; BL1 sees the mailbox and starts BL2, which reports **0x11 = 0x2300**
  and serves its web updater.
- **Leave SPI quiet while BL2 starts** [HW]. Sending HCR every 200 ms left BL2's SPI layer answering nothing. Our
  `zd1 module bl2`: power-cycle, one HCR at the first 0x2100, 15 s with no SPI traffic, then read 0x11 only
  while PA20 is high → 0x2300 ~15 s after the HCR.
- **BL1's serial menu** [HW 2026-10-23], not an SPI sequence: the module power-cycled, 0x02 on the module console
  (USART1) from the reset's release until BL1's menu appears, then `4` CR once → "Starting Bootloader Level 2" →
  0x2300 at ~11 s. Works whatever the app partition holds. `Module ESC` does it all in the MCU (`modcon.c`, the menu
  and BL1's start matched in the console's bytes; `zd1 module esc`) [HW 2026-11-02]; until then the host drove it over the MCU's CDC port after `Module HOLD` [HW
  2026-10-23]. From the host over the host link, 0x02 only after HOLD's reply came too late for BL1's window in the
  emulator.
- Back to the app: a module power cycle (`mod off`, `mod power`); BL1 cleared the mailbox when it read it.

## Module firmware through the MCU bootloader (`zd1 dfu relay`) [HW]
DFU on the MCU → `coproc-init` → the bootloader powers the module, sends HCR 0x2120 once the image is ready, waits
for BL2 (coproc status 0x12) → HCR 0x2220 → XModem-1K over 0x2C (module → MCU: 'C'/ACK/NAK) and 0x2D (MCU → module:
frames) → end of transfer → BL2 has written the image as it arrived. On the unit with the stock app and with our OS
(`docs/mcu/bootloader-dfu.md`). Our MCU app does the same transfer itself (`zd1 xmodem`: HCR 0x21,
then 0x2220).

## Telnet shell on and off (stock module) [HW]
- On: V-write 0x75 = port as ASCII (8 bytes, e.g. "23"), then F-write 0x76 = `05 00`. Off: 0x77 = "50001", 0x76 =
  `04 00`. Our firmware: `zd1 reg write` (`docs/module/knos.md`). Too soon after ready it has no effect; a few seconds later it works.

## Module asks the MCU to update itself (0x11 = 0x27) [RE]
Stock on variant-2 boards: set ISP_FORCE, GPLP flag 0x1000000, mute, module hard-off, 200 ms, watchdog reset → MCU
DFU. Variant 1 (ours) ignores it.

## Shutdown [RE]
Graceful: W 0xE2 = `00 00`, wait ≤ 5 s for a 0xE2 notification with b0 = 1, then PA17 low, 20 ms, PA04 low, SPI pins
to inputs. Hard (standby with a module power-off, watchdog): no handshake.

## Our OS: the native link
Our MCU firmware drives the module as a source over the native registers only (`mcu/src/wifi.c`), when the module
answers this firmware's version on 0xD5 (`zd1-linkd`: `spec::native::PROTO`). The stock app, or our OS with another
version, gets the base layer alone (the hello, the slot, the update, shutdown, HCR; `docs/web.md`, Pairings) and is
reported to the host as a state, not driven in its own dialect: the sections above describe the stock pair, not this
firmware. Registers: `registers.md`.
1. **Hello.** At the module's ready, and whenever its status is notified again without a boot (our link started
   again: `zd1-linkd` restarted), the MCU reads 0x25 and 0xD5, then writes 0xC3 (its version), 0xC4 (the slot), 0xC1
   (the debug mode, once set) and **0xD2**, the speaker: the volume step, the cap (the highest step a module may ask
   for, 40), whether the module is the input. Before 0xC3, `zd1-linkd` queues only what a stock host knows. After
   0xC3 it notifies its receivers (0xC6) and its State record (**0xD6**: the host link's `State` message;
   `docs/module/settings.md`), the record again whenever it changes, once a second at most.
2. **The speaker** (0xD2) is written again whenever any of it changes: entering / leaving the module's input
   (instead of DF + E2 / DE + E1), a volume change on the speaker, and after every volume request.
3. **Volume**: the module asks with **0xD4** (b0 a step, 0..cap); the MCU applies it while the module is the input,
   else keeps it for when it is (a later knob change wins), and answers with 0xD2 even if nothing changed. No echo
   rule: 0xD2 is what the speaker does.
4. **Keys**: **0xD3** (b0 play/pause, next, previous; b1 counts) only for presses while the module is the input. A
   person choosing Wi-Fi (ON/OFF, the play key in standby) chooses the receiver in front on **0xC2** instead of DF.
5. **Play state**: the receiver in front and its state (0xC6) instead of the 0x47 XML: playing switches the source
   (stock's auto-switch) and wakes the speaker. A pick (0xC0) switches at once, as a choice made by hand.
6. **V writes are acknowledged.** The module echoes every write (`02 reg 00 00` for a V write, as the stock protocol
   has it); to our OS the MCU reads the echo after each V write and, without one, sends the write again (three
   tries, 150 ms apart). `zd1-linkd` drops a request left half received after 100 ms of silence. Why: the slave's
   RX FIFO (64 bytes) overruns when `zd1-linkd` is away for a tick, and a V write arriving short left the parser
   waiting for a bogus length, swallowing everything after it: the link was dead until `zd1-linkd` restarted
   [HW 2026-10-31: `Update` BEGIN one second after the module's ready, its services still starting]. An `Update`
   BEGIN given up after the three tries fails at once ("didn't acknowledge"). `zd1-linkd` keeps its loop free of
   anything that waits (the supplicant's requests run in a thread), and the MCU sends a long write to our OS 32 bytes
   per 15 ms: other processes still preempt `zd1-linkd` for a scheduler slice while the services start. BL2 and the bootloader's relay get no echo
   check.
On the unit [HW 2026-10-28]: the hello at boot and after `zd1-linkd` restarts, volume both ways, keys, play/pause
(`status.md`). The relay to BL2 against our OS gets BL2-ready [EMU 2026-10-28, `zemu run --mcu`,
`zd1 dfu relay --handover-only`].
