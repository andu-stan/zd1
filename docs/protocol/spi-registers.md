# MCU ↔ module: the SPI register protocol

> The registers our code uses are defined in `protocol/spi/src/spec.rs`, the single source for both sides (generated
> reference: `docs/protocol/registers.md`). This file keeps the evidence and the stock firmware's behaviour.

The only control link between the main board and the CR870 module: the MCU is SPI master, the module serves a file
of numbered registers. Reconstructed from both sides' code (MCU `dmhBcoMgr`/`BcoZD1`/`BcoWrapper`; module register
table at app 0x60589e80, `re/netmodule/spi_register_map.txt`). On the unit, our MCU firmware drives the stock module
app, BL2 and our module OS with it (power-up, notifications, AirPlay, volume, HCR, telnet) [HW]; the stock MCU
firmware against our module code has only run in the emulator. The vendor spec
(`DeviceControlProtocol_Registers_v_2_1.pdf`) was never public.

Tags (`docs/README.md`): [RE] read from code, [HW] seen on the unit, [EMU] emulator only, [INF] inferred. Addresses
are MCU flash (0x8…) or module app (0x6…) addresses.

Wire convention: a "16-bit" (F) register is two bytes on the wire, written here as `b0 b1` in transmission order.
The MCU treats **b0 as the high byte**: status 0x2100 is `21 00`.

Sequences built from these registers (power-up, play, source switch, BL2, telnet, shutdown): `sequences.md`.

---

## 1. Link layer (recap)

### 1.1 Physical [HW]

| Item | Detail |
|---|---|
| Bus | SPI0 master, SPI mode 3 (CPOL=1), 8-bit. Stock: SCBR 15 (~4.4 MHz at 66 MHz). Ours: 3 MHz; 800 kHz behaves the same. The CR870 datasheet recommends ≤ 2 MHz [RE] |
| Chip select | NPCS2 on PA09, asserted for each request |
| Module power | PA04, active high |
| Module reset | PA17, low = reset; left as an input while the module is off |
| Request line | PA20 from the module (DM870 GPIO 6), **level-polled**, high = the module has queued data. No interrupt is used. |

### 1.2 Byte stuffing [HW]

Both directions use the same stuffing: data 0x27 is sent as `27 27`, data 0xFF as `27 01`. A raw 0xFF on the wire means idle.

### 1.3 Request formats

A 256-entry type table at 0x8000a27c marks each register as F (fixed 2-byte) or V (variable).

| Request | Bytes | Notes |
|---|---|---|
| Write F | `00 reg b0 b1` | CS high right after. The MCU never reads the module's write acknowledgement. |
| Write V | `02 reg lenLo lenHi data…` | len ≤ 0x405 |
| Read | `01` (F) or `03` (V), then `reg 00 00` | Then CS high, wait 5 ms, CS low, clock 0xFF. |

Read response parsing:
1. Skip bytes until the type byte (500 ms timeout).
2. Skip bytes until the register byte (500 ms timeout).
3. F: take 2 bytes. V: take `lenLo lenHi` (little-endian), then the data (5 s timeout).

That is stock's; our firmware gives the whole reply one 300 ms deadline, and a V write's echo 300 ms (it reads them
from our OS: `sequences.md`), so that every wait stays well inside its watchdog.

Return codes: 0 = OK, 0x420021 = busy (retry later), 0x420022 = timeout.

The module answers every request (writes too: `type reg lo hi`, or `type reg len_lo len_hi data` for V), but the MCU
never reads write replies. Every byte is stuffed, headers too [RE 0x8000c050 / 0x8000c154, `protocol/spi/src/framing.rs`].

### 1.4 Notification drain

This runs in state 10/11 of the IRQ state machine 0x8000ca5c [RE].

While PA20 is high:
1. Read **reg 0x0C** (F). b0 is the number of the register that has pending data.
2. Read that register (V-style read, ≤ 0x406 bytes).
3. Dispatch through the table at 0x8000b2b4 (section 3). Registers not in the table are read and dropped.

Reg 0x00 (queue pop: the oldest queued message, an empty V reply when none) is never used. Reading a register that
has a queued message returns that message and removes it. One logical message (e.g. 0x47 XML) can arrive as several
notifications of ≤ 256 bytes, a chunk may start mid-tag. [RE]

---

## 2. Boot / handshake sequence (module power-up to "ready")

### 2.1 Power and attach

Attach state machine 0x8000dd58 [RE]:

| Step | Action |
|---|---|
| A0 | Initial state (0x8000da88): PA17 = 0, PA04 = 0. |
| A1 | Attach requested (event 0x1000000 via 0x8000e160): **PA04 = 1**, start a 1200 ms timer. |
| A2 | After 1200 ms: **PA17 = 1**, PA09/11/12/13 switched to SPI function, write CSR2 then MR, enable SPI, start a 200 ms timer. |
| A3 | After 200 ms: start the register-access task (0x8000c6ec), the IRQ state machine (0x8000ca5c state 2), Wi-Fi setup (0x80009c38 state 1), play-status (0x8000bccc), site survey (0x8000bea0) and NFU tasks. Notify id 0 ("attached"). The network is marked attached and the Wi-Fi status set to 1. |

### 2.2 IRQ state machine

State variable at RAM 0x3ed4+0x4b0. Timer T3 is a short timer; T17 is a 60 s supervision timer [RE].

| State | Behaviour |
|---|---|
| 2 (start) | T3 = 1000 ms, T17 = 60 s, retry counter = 0, "failure event" = 0x800000. Go to 4. |
| 4 | Wait for T3, then **wait for PA20 = 1**. Go to 5. |
| 5 | When T3 expires, go to 6. |
| 6 | **Read reg 0x11** (2 bytes). Busy: stay. Timeout: T3 = 300 ms, back to 5. On success, re-arm T17, then decode the value as below. |
| 8 | T3 = 3000 ms, go to 9. |
| 9 (idle) | PA20 = 1: go to 10 (drain). T3 expired: go to 7 (heartbeat). |
| 7 (heartbeat) | Read reg 0x11 again. The value is only used to re-arm the supervision timer; the notification path below is what acts on status changes. Back to 8. |
| 10 / 11 | Notification drain (section 1.4). When PA20 drops: if status = 0x2100 go to 8, otherwise go to 6. |
| any waiting state | When T17 (60 s) expires: retry counter + 1, re-arm 60 s. If the counter exceeds 30 (about 31 minutes with no successful read), go to state 3. |
| 3 (error) | Post the current failure event, power the module off, IRQ state machine idle. |

Value decoding in state 6:

| Reg 0x11 value | MCU action |
|---|---|
| 0x2000, 0x2200, 0x2400, 0x2500 | Store as status, go to 8. The module is **not** usable: register reads and writes are only performed while status = 0x2100, so the MCU keeps re-reading 0x11 whenever PA20 is high. |
| **0x2100** | Status = 0x2100 (**ready**). Failure event becomes 0x10001000. If a detach is pending (event 0x2000000), do the detach now (0x8000db30). Otherwise **notify id 2 ("module ready")**. Go to 8. |
| **0x2300** | Treated as ready: status = 0x2100, plus notify id 5 (ZD1: LED pattern 22, red blink; network-settings sync suppressed). Go to 8. |
| anything else | T3 = 20 ms, status = 0, back to 5 (fast retry). |

**Failure events and notify ids** (dmhBcoMgr event table at 0x8000d8c4) [RE]:

| Failure | Event | Notify id | Effect |
|---|---|---|---|
| Never got a status | 0x800000 | 0x24 | Wi-Fi status 6, system event 0x60000004 = **system error 5** |
| Lost after ready | 0x10001000 | 0x26 | Wi-Fi status 5, system event 0x70000004 = **system error 10** |
| Lost after first notification drain | 0x10000800 | 0x25 | Wi-Fi status 7, system event 0x70000002 = **system error 11** |

A system error means amp mute, power stage off, red blinking LED (pattern 22) and ERROR mode (0x800235fc). The exception is factory mode.

### 2.3 Wi-Fi setup task after ready

Task 0x80009c38; jump table at 0x80009278 [RE]:

| Step | Action |
|---|---|
| S2 | Wait until status = 0x2100. |
| S3 | **R 0x25** VersionImage (≤ 20 bytes). Spaces are removed in place; the result is stored at RAM 0x3a74+0x18. |
| S4 | **R 0x26** VersionBootld, same processing, stored at +0x38. |
| S5 | **R 0x27** VersionCNE, same processing, stored at +0x04. |
| S6 | Configuration **R "pssCne/CneId"** (op 'R' on reg 0x45; reply ≤ 10 bytes). |
| S7 | Configuration **W "SystemHost/HostVersion" = "2.04.05"**. This is the MCU version string from 0x80011228 / RAM 0x4f8c; board variant 2 sends "2.04.35". Post-delay 300 ms. |
| S8 | **R 0x32** NetStatus (2 bytes) into RAM 0x7a. |
| S9 | **Notify id 3 ("info ready")**. Then idle (S10), reacting to events: WPS, JBConnect, factory reset, shutdown. |

**Notify id 3 has a firmware-version gate** (observer 0x8001e9f8 case 3, compare at 0x80009340) [RE]:
- The space-stripped VersionImage string, **from character index 13 for 4 characters**, must equal "9112". 9112 = 0x2398 is the module SDK build. The stock module returns "I2014092908219112" (+ NUL); 0x26 = "B201009271404 081008" (no NUL), 0x27 = "C 2014092908219112" (+ NUL). [RE]
- If not, the MCU posts system event **0x10002000 = system error 7** (mute, standby, red blink).
- If it matches, it starts the network-settings sync task (0x8001d36c, see section 4.3). That is skipped while reg 0x11 = 0x23.
- A replacement module **must** return a VersionImage that passes this check, for example padding so that stripped[13..16] = "9112". (Our module OS doesn't: it runs with our MCU firmware only.)

### 2.4 Graceful shutdown

Wi-Fi setup S11 → S12 → S13 [RE]:
1. Write **reg 0xE2 = `00 00`**.
2. Wait up to 5 s for a notification on reg 0xE2 with b0 = 1 (0x8000b438, event 0x20).
3. Detach: PA17 = 0, wait 20 ms, PA04 = 0, SPI pins become inputs, notify id 1.

Hard power-off (0x8000dcdc, used for standby and watchdog resets): no handshake.

### 2.5 Module-requested MCU update (reg 0x11 = 0x27)

See section 3.1. The MCU sets ISP_FORCE and reboots into its DFU bootloader.

---

## 3. Register-by-register semantics

### 3.1 0x11 DM1000Status (F, MCU reads and is notified)

- **Polled** in the boot state machine: values 0x20–0x25 in b0 (section 2.2).
- **Notified** (handler 0x8000b684): acts **only when b1 = 0x00**.

| b0 | Event / notify | MCU action | Conf |
|---|---|---|---|
| 0x20 | – | Not handled when notified. When polled: stored, the module is treated as not ready (booting). | RE / INF |
| **0x21** | status := 0x2100 | Module application ready; API unlocked. | RE |
| 0x22 | – | Polled: stored, not ready. | RE |
| 0x23 | ev 0x10000010 → notify 5 | Observer #2 (0x8001e134): LED pattern 22 (red 100 ms blink), ZD1 event 0x40000 (suppresses the network-settings sync). When polled it is also treated as ready (0x2100). **= BL2 running** [HW 2026-10-19]. | RE |
| 0x24 | – | Polled: stored, not ready. | RE |
| 0x25 | ev 0x10000002 → notify 7 | Observer #1 default case: clears the "AirPlay playing" flag; no other action. | RE |
| 0x26 | ev 0x10000008 → notify 0x29 | Ignored by both observers. | RE |
| **0x27** | ev 0x10000004 → notify 8 | **Only on board variant 2** (ADC2 ID ≥ 0x200): set GP fuse bit 31 (**ISP_FORCE**) via 0x8001be0c/0x8001bd94, set GPLP flag 0x1000000, amp mute (PB16 low), hard-off the module, wait 200 ms, **watchdog reset → MCU enters the USB-DFU bootloader**. Variant 1: no action. This is how a network-initiated update relays firmware through the bootloader. | RE / INF |

### 3.2 0x32 NetStatus (F, two ASCII characters; MCU reads and is notified)

- Read once at Wi-Fi setup S8 into RAM 0x78+2/+3.
- Notification handler 0x8000b518 [RE]: the new value is compared with the previous b1 and both bytes are stored.

| Condition | Event | Notify | Observer #1 effect |
|---|---|---|---|
| prev b1 '1' → new b1 '0' | 0x10000 | 0x1B | Wi-Fi status := 1 |
| prev b1 '0' → new b1 '1' | 0x8000 | 0x1A | none |
| prev b1 '2' → new b1 '0' | 0x40000 | 0x1D | none |
| prev b1 '0' → new b1 '2' | 0x20000 | 0x1C | none |
| **b0 ≠ '0' AND b1 ≠ '0'** | 0x80000 | **0x1E** | "connected": network state := 2, Wi-Fi status := 2. Only applied while not in JBConnect setup (RAM 0x738c[0x68] = 0). |
| otherwise | 0x100000 | **0x1F** | "not connected": network state := 1, Wi-Fi status := 4 |

Observed [HW]: "21" on **wired** Ethernet, so b1 is not the interface. Stock's rule (both ≠ '0' = connected) is all that
matters. Default `30 xx` = not connected. [RE]

**How network state drives the LED** [RE]. Source manager 0x800248bc, getter 0x8001e738 = network state:
- When the Wi-Fi source is selected:
  - state = 2 (connected): LED **pattern 7 (purple, solid)**; the module is told DRC 0xDF + 0xE2 (section 3.6). Sub-state 0x15.
  - otherwise: LED **pattern 25 (purple 400 ms on / 400 ms off)** until it becomes 2. Sub-state 0xF.
- JBConnect setup mode: LED **pattern 23** (yellow 200/200/200/1000 ms).
- While the module boots (main mode BOOTING, 0x80012d74): LED **pattern 33** (red "breathing") until Wi-Fi status = 2, at most 1300 ms of startup delay first.
- Per-source colours (0x800247a8 → LED): 0 → blue (pattern 3), 1 → green (2), 2 → orange (5), 3 = Wi-Fi → purple (7).

Wi-Fi status values (RAM 0x7afc+4): 0 off, 1 attached/booting, 2 ready-idle, 3 AirPlay playing, 4 not connected, 5/6/7 link errors.

### 3.3 0xE3 AirTunesPlaybackStatus (F, notified only)

Handler 0x8000b800, which calls AirPlay service callback 3 = 0x8001e98c (`cbRegDacpCmdAckHandler`). Only b0 is used [RE].

| b0 | Meaning on the MCU side | Effect |
|---|---|---|
| 0x00 | Ack of **DRC 0xDF** ("play/resume", M) | Event 8 → DRC state machine 2 completes |
| 0x01 | Ack of **DRC 0xDE** ("pause", M) | Event 0x10 |
| 0x04 | Ack of **DRC 0xE1** ("release audio path / AirPlay off", M) | Event 0x80, plus system event **0x70010000**. The iPod manager (0x80028b00/0x80028d30) waits up to 3 s for this before starting iPod audio. |
| 0x05 | Ack of **DRC 0xE2** ("re-acquire / AirPlay on", M) | Event 0x100 |
| other | ignored | |

Play / pause / stop state is **not** taken from 0xE3. It comes from **reg 0x47** (DRCProtocol XML, parser callback 0x8000d36c) [RE]:
- The XML path is `view.id.status` or `update.id.status`, then element `icon.id.play` with text `pl`, `pa` or `em`. For example `<update id="status">…<icon id="play">pl</icon>…</update>`.

| Text | Status | Notify | Effect |
|---|---|---|---|
| `pl` | 2 (playing) | 0x16 | Wi-Fi status := 3, "AirPlay playing" flag set |
| `pa` | 3 (paused) | 0x17 | Wi-Fi status := 2 |
| `em` | 1 (empty / stopped) | 0x18 | Wi-Fi status := 2 |

**AirPlay auto-switch.** If `pl` arrives while DRC state machine 2 is idle (0x8001f150, flag RAM 0x7b0c):
- main mode SLEEP → wake onto source 3 (`SLPS_voActivateSource 3`, 0x80010594);
- main mode RUN → system event 0x60000001 → switch to the Wi-Fi source.

### 3.4 Volume: 0x40 and 0xE4 VolumeHostChange (V, 4 bytes, both directions)

Reg 0x40 is not in the module's static table; it is added at run time. Right after the module is ready it sends
0x40 = `00 00` (2 bytes, not the 4-byte form). [RE] Both registers use the same 4-byte payload `[svc][param][x][val]` and the same handler 0x8000b32c [RE]:
- `svc` 0x03 = AirPlay service (callbacks at RAM 0x0c2c: cb0 = 0x8001e9ec, cb2 = 0x8001e774, cb3 = 0x8001e98c).
- `svc` 0x01 = DLNA service (not registered in this build, so ignored).
- `x` (byte 2) is ignored on receive.

| Module → MCU | Callback | MCU action |
|---|---|---|
| `03 03 x vv` | cb0 → 0x80013960 | **AirPlay volume** (below) |
| `03 01 x 00` | cb2 → 0x8001e774 | MCU replies (W 0x40) `03 01 01 02` |
| `03 01 x 03` | cb2 | MCU replies `03 01 01 04` |
| `03 01 x other` | – | ignored |
| `01 04 x v` | (svc 1 cb1) | not registered |

The `03 01` exchange is acknowledged only; it never changes the MCU's source or volume. A third reply, `03 01 01 01` (event 0x400), has no setter in this build [RE]. Meaning unknown [INF, guess]; it looks like a status / ownership handshake.

**Volume scale** [RE]. `vv` is an attenuation: 0 = loudest, 0xF0 = quietest, **0xF1–0xFF = mute** (MCU sends 0xFF for mute).

- **Module → MCU** (0x80013960):
  - vv ≤ 0xF0: MCU step = `0x47 − ((vv + 3) · 70 / 240)` (integer division), so 0x47 = max, about 1 = min.
  - vv > 0xF0: step 0 (mute).
  - The step is clamped to the per-source maximum (EEPROM byte RAM 0x7f98[0xC+3]).
  - It is applied (DSP safeload to param 0x4B) only when the current source is 3 (Wi-Fi) and the AirPlay path is active. Otherwise the value is stored and applied when Wi-Fi becomes active (0x80013a00).
- **MCU → module** (0x8001cf6c, sent by 0x8001dc84 as W 0x40 `03 03 01 vv`, on every local volume change while the module is enabled):
  - DSP index d = 71 − step (0 = +10 dB … 70 = −60 dB, 71 = mute).
  - `vv = d · 240 / 70` for d < 71; `vv = 0xFF` for d = 71 (mute).
  - Round trip: MCU step s ↔ vv ≈ (71 − s)·3.43. One MCU step (1 dB in the DSP) is about 3.4 vv units.

The MCU's 72 steps map to the DSP master-volume table (`re/mcu/extracted/dsp/runtime_tables.txt`).

### 3.5 0x05–0x08 StreamAttributes / AudioFormat / Channel / Frequency

**Not used by the MCU** [RE]:
- They are typed F in the MCU table, but there is no read or write call site and no notification handler. If notified, they are read and dropped.
- The Wi-Fi audio path is fixed: source 3 → DSP input select 2 (E080 = 0x0002, E002 = 0x0908; the module drives the I2S clocks) and `FUN_80029894(44100)` (0x800298c8).
- The CS2000 is **not** reprogrammed for Wi-Fi: its gate RAM 0x737c is cleared by 0x8001cab8. The DSP is not re-downloaded.
- So a sample-rate change on the module side is not followed by the MCU. AirPlay is always 44.1 kHz [INF]. The Frequency default 0x2038 encoding was not decoded (the MCU never uses it).

### 3.6 HCR 0x28/0x29, DCR 0x2A/0x2B, DRC 0x48

**HCR Ctrl (W 0x28, F `b0 = cmd, b1 = arg`)**: senders 0x8000bbfc (plain) and 0x8000bb20 (with retry: 300 ms timer, post-delay) [RE].

Module side (handler 0x602f7310; status on 0x29: `20 20` OK, `21 21` fail) [RE]:

| Command | Arg | Module action |
|---|---|---|
| 0x20 | – | notify observers |
| **0x21** | – | **reboot into BL2** ("start bootloaderL2 to update application"): BL2 mailbox + watchdog reset (`docs/module/boot-chain.md`). Our MCU firmware's `zd1 module bl2` and the MCU bootloader's relay use it. |
| 0x22 | 0x26 / 0x27 | check online / a USB stick for firmware |
| 0x23 | 0x20-0x25 | flash firmware already transferred into RAM |
| 0x24 | 0x20 / 0x21 | system restart / settings-flag action |
| 0x25 / 0x26 | – | file transfer over the XModem registers 0x2C/0x2D |

What the stock MCU app sends:

| Code | When | Meaning |
|---|---|---|
| **0x2421** | Factory reset (FRS mode → 0x8001e754 → 0x800093fc → Wi-Fi setup S16), then notify 0x19 → graceful detach | cmd 0x24 arg 0x21: module "settings flag" / reset network settings [INF] |
|  | **BL2 handles 0x2421 too** (HCR dispatch 0x6005ffa0 case 0x24/'!' → 0x60073254): no console output, but the user-settings area (NAND blocks 3/4) ends up byte-identical to what the app's own factory reset leaves (fresh header, settings gone) [EMU, the scenario suite up to b31219b]. B&W's recovery-utility MCU image sends it to the app (waits for 0x2100 first) | RE |
| 0x2520 | XModem GET_FILE (0x80008ba0) | File transfer. Only reachable from the NFU task, which is **dead code** in this build. |

No other HCR codes are sent. The MCU application never sends 0x21 / 0x22 / 0x23 (update) commands.

**HCR Status (notify 0x29)**, handler 0x8000b60c: stores b0b1 at RAM 0x3ed4+8.
- b0 = 0x22 → event 0x10010000.
- 0x2020 / 0x2120 → event 0x10004000.
- 0x214E → ignored.
- else → event 0x10008000.

Only the dead NFU task consumes these events, so in practice **HCR status is ignored** except for logging [RE].

**DCR Ctrl (notify 0x2A)**, handler 0x8000b5e0: **only 0x2423 is recognised** (event 0x12000000, consumed only by the dead NFU task). The MCU never writes 0x2A or 0x2B [RE]. So no DCR request from the module produces an action; for example `24 22` "USB stick" is ignored.

**DRC Command (W 0x48, F `[code, 00]`)**:
- Plain sender 0x8000bc64; with retry 0x8000bb90.
- At most one DRC from the event table is sent per main-loop tick.

| Code | Trigger (MCU) | Expected ack | Meaning | Conf |
|---|---|---|---|---|
| **0xB0** | Key/IR **Next** (event 0x50000001) while on the Wi-Fi source (0x80025aca → event 0x10000200) | none | next track | RE trigger, INF meaning |
| **0xB1** | Key/IR **Previous** (0x50000002) on Wi-Fi (→ event 0x10000400) | none | previous track | RE / INF |
| **0xB3** | IR **Play/Pause** (0x40000010) on Wi-Fi (→ event 0x14000000) | none | play/pause toggle | RE / INF |
| 0xB2, 0x13, 0xFA | Table entries (events 0x11000000, 0x10800000, 0x10400000) with no setter in this build | – | unknown (stop? mute?) | RE: unused |
| **0xDE** | Leaving the Wi-Fi source (other source selected, standby, iPod docked), via DRC state machine 2 (0x8001f150) | 0xE3 = 1 | pause AirPlay | RE / INF |
| **0xDF** | Entering the Wi-Fi source (0x80025dc6) or startup resume | 0xE3 = 0 | play/resume AirPlay | RE / INF |
| **0xE1** | Leaving Wi-Fi for a local source / standby (DRC state machine 1, 0x8001ed74) | 0xE3 = 4 | release / disable AirPlay output | RE / INF, guess |
| **0xE2** | Entering the Wi-Fi source, and at boot | 0xE3 = 5 | (re)enable AirPlay output | RE / INF, guess |
| **0x9D** | JBConnect start (after writing Cne `Networking/JBCONNECT/JBCSSID`) | 0x6A progress | start setup network | RE / INF |
| **0x9F** | JBConnect stop | – | stop setup network | RE / INF |
| **0x9E** | Wi-Fi setup S17 on BCO event 0x10, posted when Cne `Application/NetIF/mode` was set to "wireless" (0x8001dc84 → system event 0x70004000 → source manager) | – | re-apply / restart wireless | INF |

**Timing for DE / DF / E1 / E2** [RE]:
1. Wait 2 s (timers 0x2F / 0x3B), then send the DRC.
2. Wait up to 3 s for the matching 0xE3 value.
3. On timeout, return to the request state and resend while the request is still pending.
4. After the ack, wait a caller-specified delay (minimum 10 ms) before going idle.

### 3.7 0x45 Configuration channel (V)

**Request** = W 0x45 `[op][nameLen][name…][value…]`: ASCII, no NUL, the name must not start with '/'. Replies arrive by notification on 0x45 (handler 0x8000b458 → event 0x10200000), then R 0x45 (≤ 0x100 bytes) [RE].

| Op | Function | Reply |
|---|---|---|
| 'P' | GetPersParamInteger (0x8000a9a4), name exactly 4 chars | Hex text, parsed with strtoul base 16. 8-byte values use a second hex field at +11 (`0x%08lX:0x%08lX`). |
| 'Q' | GetPersParamString (0x8000a764) | raw string |
| 'R' | GetCneValue (0x8000d614), 1 s timeout | string |
| 'U' | SetPersParamString (0x8000a5cc) | none; the MCU waits a fixed time |
| 'V' | SetPersParamIntegerHex (0x8000a428) | none |
| 'W' | SetCneValue (0x8000d488) | none |

**Names used by the MCU** [RE]:

| Op | Name | Value / purpose | Where |
|---|---|---|---|
| R | `pssCne/CneId` | read at boot (S6); stored, version info | 0x80009e18 |
| W | `SystemHost/HostVersion` | "2.04.05" (MCU firmware version) | 0x80009dfe |
| W | `Application/NetIF/mode` | `wired` / `wireless` / `autosense`; set by the source manager on network setup or factory reset (0x8001cf20 → 0x8001dc84). If "wireless": system event 0x70004000 → DRC 0x9E. | 0x8001dd9c |
| R | `Application/NetIF/mode` | read in the network-settings sync | 0x8001d778 |
| W | `Networking/JBCONNECT/JBCSSID` | `ZeppelinAir_Setup_XXXXXX` (XXXXXX = last 6 hex digits of the MAC from reg 0x33) | 0x800099c6 |
| V | `MAC0` | factory: MAC as `0x%02lX%02lX…` (OUI 00:09:82) | 0x8001de46 |
| U | `FPIN` | factory: 8-char fixed WPS PIN | 0x8001de62 |
| U | `CTRY` | factory: 2-char country code (`EU` / `US` / `JP`) | 0x8001de7e |
| P | `MAC0`, `HWID` | read in the sync (MAC, hardware ID) | 0x8001d65e / 0x8001d6ae |
| Q | `CTRY`, `FPIN` | read in the sync | 0x8001d680 / 0x8001d8dc |
| W | 24 × `Networking/...` | Restore of the network-settings backup (paths in table RAM 0x628). The paths are `Networking/WPS/IES/FixedPin`, `.../WlanCfg/ActiveProfile`, `.../Profile1/{Security, IBSS, SSID, IpConfig, WEP/Authentication, WEP/KeyIndex, WEP/Key0–3, WPAPSK/Cipher, WPAPSK/Passphrase}`, `Networking/IpCfg/DeviceName`, `.../Config1/{DNSSrv1, DNSSrv2, ProxyServerPort, ProxyServer, Proxy, DefGW, Mask, IP, DHCPClient}`. | 0x8001dac0 |

Without reg 0x33 the stock MCU writes `ZeppelinAir_Setup_` with no suffix [EMU].

The `MAC0` / `FPIN` / `CTRY` writes are factory-UART-driven (`webdata` settable keys snum / macaddr / fixpin / ccode).

### 3.8 Other registers the MCU touches

| Reg | Dir | MCU use | Conf |
|---|---|---|---|
| 0x0C | R | pending-register index (b0) | RE |
| 0x25 / 0x26 / 0x27 | R | version strings; 0x25 is gated on "9112" (section 2.3) | RE |
| 0x2C / 0x2D | N, R / W | XModem receive (MCU = receiver, `C`/ACK/NAK/CAN control bytes on 0x2D). Only the dead NFU path uses it. | RE |
| 0x2E | R | WebpagesVersion, read in the settings sync | RE |
| 0x33 | R | MAC string `xx:xx:xx:xx:xx:xx` (18 bytes), for the JBConnect SSID | RE |
| 0x35, 0x37–0x3F, 0x58, 0x5A–0x66 | R | Network-settings backup (sync task 0x8001d36c). Reg 0x37 DHCPClient: 1 = DHCP, otherwise static. Before reading profile-specific registers the MCU writes 0x5B (SelectedWirelessProfile). | RE |
| 0x38 | N | IP string (16), copied to RAM 0x68 | RE |
| 0x58 | N | DeviceName → notify 0x28 (flag "name changed") | RE |
| 0x5A | N, W | ActiveWirelessProfile: notification b0 − '0' stored; written as `[('0' + n), 00]` after WPS success | RE |
| 0x68 | W | BssScan (site survey start, 2 bytes) | RE |
| 0x69 | N, R | BssScanResult XML (`bss_lis.count`, `bss.index`, `ssid`, `ibss`, `channel`, `quality`, `phytype`) | RE |
| 0x6A | N | BssAttemptConnection / WPS / JBC progress (b1): `'0'` / `'B'` success (b0 = profile) → write 0x5A; `'1'`, `'2'` → retry/fail; `'3'`–`'5'` → fail codes; `'6'` → event 0x10002000; `'@'` → JBC connected | RE decode, INF meaning |
| 0x6B | N | b0 = 2 and b1 ≠ 0 → notify 0x27 (signed b1), which is **ignored** | RE |
| 0x39 / 0x3A | N | netmask / gateway strings, not in the MCU dispatch table (read and dropped) | RE |
| 0x56 / 0xD1 | N | `3c 00` / 256 zero bytes after ready, meaning unknown | RE |
| 0x6C | W | WPSCommand: `[profile#=0x01]["WPS_CFG" (PIN) or "PB_CFG" (push-button)]\0` | RE |
| 0xDD | W | Web status page XML: `<update id="webdata">\r\n <value id="k">v</value>… </update>` (keys snum, macaddr, fixpin, ccode, hostver, hblver, dspver, dsp2ver, hwver, hwid, imgver, cnever, bslver, htmver, pwrtemp, amptemp). The trigger (observer #2 id 0x23) has no caller in this build, so it is never sent. | RE |
| 0xE2 | W, N | shutdown handshake (section 2.4) | RE |

Never touched by the MCU: 0x00, 0x10, 0x13, 0x05–0x08, 0x2A (write), 0x2B, 0x2F, 0x30, 0x31, 0x34, 0x36, 0x41/0x42, 0x46, 0x49–0x57, 0x74–0x7B, 0x93, 0xCE/0xCF, 0xD0, 0xE1, 0xE5/0xE6, 0xF2–0xF6 [RE].

---

## 4. Checklists for reimplementation

### 4.1 New MCU firmware talking to the stock module (minimum)

1. Bring up the link as in sections 1 and 2.1: PA04 = 1, wait 1.2 s, PA17 = 1, SPI mode 3 at about 4 MHz, wait 0.2 s + 1 s.
2. Wait for PA20, then poll reg 0x11 until b0 = 0x21 (or 0x23). Keep draining 0x0C notifications whenever PA20 is high, and re-read 0x11 every 3 s.
3. (Optional) R 0x25–0x27, then W config `SystemHost/HostVersion`.
4. Volume:
   - Receive `03 03 x vv` on 0x40/0xE4; mute if vv > 0xF0; otherwise convert vv to your own scale.
   - Send local volume changes as W 0x40 `03 03 01 vv` (vv = attenuation 0–240, 0xFF = mute).
   - Answer `03 01 x 00` with `03 01 01 02` and `03 01 x 03` with `03 01 01 04`.
5. Play state: parse reg 0x47 XML (`icon id=play` = `pl` / `pa` / `em`). Auto-switch to Wi-Fi on `pl`.
6. Transport keys while on Wi-Fi: DRC 0xB0 (next), 0xB1 (prev), 0xB3 (play/pause).
7. Source switching:
   - Leaving Wi-Fi: DRC 0xDE (ack 0xE3 = 1) and 0xE1 (ack 0xE3 = 4).
   - Entering Wi-Fi: DRC 0xDF (ack 0) and 0xE2 (ack 5).
   - Route DSP input 2; I2S at 44.1 kHz comes from the module.
8. Network setup: WPS via 0x6C; JBConnect via config JBCSSID + DRC 0x9D/0x9F; progress via 0x6A; connection state via 0x32.
9. Shutdown: W 0xE2 `00 00`, wait for notify 0xE2 b0 = 1 (≤ 5 s), PA17 = 0, wait 20 ms, PA04 = 0.
10. Decide what to do on 0x11 = 0x27. The stock behaviour is to reboot the MCU into DFU (ISP_FORCE).

### 4.2 A replacement module (our OS, or any other) talking to the stock MCU

**Must:**
- Act as an SPI mode-3 slave with the stuffing in section 1.2. Return 0xFF when idle.
- Keep a read response available until the MCU clocks it out after its 5 ms gap. Tolerate CS rising right after write headers; the MCU does not read write acks.
- Drive PA20 high while notifications are queued, and answer reg 0x0C with the next pending register number.
- Report reg 0x11 = `21 00` within about 30 min of power-up. Then any later timeout leads to system error 10 or 11; never answering leads to system error 5 at about 31 min.
- Return a reg 0x25 VersionImage string whose space-stripped characters [13..16] are "9112". **Otherwise system error 7.**
- Accept W 0x45 configuration writes and answer 'R' `pssCne/CneId` (the setup task waits for this reply).
- Answer reg 0x32 with two ASCII bytes. Report "connected" (both bytes ≠ '0', e.g. `31 31`) when the network is up; otherwise the Wi-Fi LED blinks.
- For AirPlay-like playback:
  - Provide I2S at 44.1 kHz on the module's audio port.
  - Push `<update id="status"><icon id="play">pl</icon></update>` on reg 0x47 when playback starts (this triggers the auto-switch / wake).
  - Push `pa` / `em` when it stops.
  - Send volume as `03 03 01 vv` on 0xE4 or 0x40.
- Ack DRC 0xDE / 0xDF / 0xE1 / 0xE2 by notifying 0xE3 = 1 / 0 / 4 / 5 respectively. Without the E1 ack, iPod start is delayed by 3 s.
- On W 0xE2 `00 00`, notify 0xE2 = `01 00` before the MCU cuts power.

- Report `21 00` only when the module stays up. On 0x2100 the MCU starts the setup task once (section 2.3); if the
  module then goes silent (a boot loader handing over to a kernel), the task never finishes and the Wi-Fi source
  stays in sub-state 0x14 (LED breathing red) even after the module comes back. Report `20 00` until then (our boot stub does).

**What the stock MCU does with a replacement module that passes these checks** [EMU, with an earlier build of our OS
that served the stock host's registers; RAM watches 0x7AFC
network state, 0x7B00 Wi-Fi status, 0x7D18 Wi-Fi source LED pattern / +4 sub-state]:
1. Setup task (versions, `pssCne/CneId`, HostVersion, 0x32), network-settings sync, DRC DF; sub-state 0x11 → 0x13.
2. Cne `Application/NetIF/mode` is "wireless" → event 0x70004000 → sub-state 0x14 → **DRC 0x9E** (wireless apply),
   graceful shutdown (W 0xE2, ack), module power-cycled (0x8000db94).
3. After the restart: the setup task again, then with no SSID stored, **DRC 0x9D (JBConnect start)**: Cne
   `Networking/JBCONNECT/JBCSSID = ZeppelinAir_Setup_XXXXXX` (from reg 0x33), LED yellow (pattern 23). The stock
   module would now open the setup access point.
4. Play state `pl` on 0x47 still gives Wi-Fi status 3 (AirPlay playing), and volume works both ways.

**Optional:**
- 0x33 MAC, 0x6A/0x6C WPS, the 0x68/0x69 site survey, and the network-settings registers read by the sync task. Each read that times out is just skipped after the 5 s timeout [INF].

---

## 5. Summary

The MCU uses a small subset of the module's register map:
- **0x11** status: 0x21 = ready, 0x23 = ready but flagged, 0x27 = "reboot MCU into DFU".
- **0x32** connection status (two ASCII bytes → connected / not connected → purple LED solid or blinking).
- **0x47** XML play status (pl / pa / em; `pl` triggers auto-switch to AirPlay).
- **0x40 / 0xE4** volume as a 0–240 attenuation scale (0xFF = mute) mapped linearly onto the 72 DSP steps.
- **0x48** DRC codes:
  - B0 / B1 / B3 = next / prev / play-pause;
  - DE / DF / E1 / E2 = pause / resume / release / acquire, acknowledged on **0xE3** with 1 / 0 / 4 / 5;
  - 9D / 9F / 9E = JBConnect start / stop and wireless apply.
- **0x45** configuration paths (HostVersion, CneId, NetIF mode, JBCSSID, MAC0 / FPIN / CTRY / HWID, 24 Networking backup paths).
- **0xE2** shutdown handshake.

HCR is only used for the factory reset (0x2421). DCR, HCR status and the audio-format registers 0x05–0x08 are effectively ignored. The Wi-Fi audio path is hard-wired to 44.1 kHz.

Two behaviours matter for a replacement module:
- The firmware-build check: VersionImage must contain "9112", or the MCU raises system error 7.
- The supervision timeouts, which lead to system errors 5, 10 and 11.

## 6. Registers our firmware adds or uses beyond stock

| Reg | Use |
|---|---|
| 0x75 TelnetShellPort (8 B, ASCII decimal) / 0x77 TCPIPSpiPort (8 B) / **0x76 UARTPortsSelect** (F) | module shell routing (handler `RemoteRegUartPortSelect`, write `FUN_6024a810`): 0x76 b0 = **5** → shell = TELNET on port[0x75] (iPod UART0, tunnel on port[0x77] or off); **4** → stock layout (shell UART1, tunnel REMOTE_REGISTERS on port[0x77]); 0 → shell UART1, iPod off. Applies live, no reboot; give the app a few seconds after "ready" first. Our firmware: `zd1 reg write` (0x75 = the port, 0x76 = 5; 0x77 = "50001", 0x76 = 4: `docs/module/knos.md`). The stock MCU patch (`stock-patch/`) re-asserts mode 5 every ~30 s. 0x41 TelnetCmd is a dead end (no handler). [HW] |
| 0x78 TCPIPToSpi | the SPI→TCP tunnel data register (live `reg print`) [HW]; MCU writes to it land in the module's default write handler [RE] |
| 0x25, 0xC0-0xC6, 0xD2-0xD5 | between our MCU firmware and our OS: its identification on VersionImage ("zd1-os VERSION KIND", the boot stub's "zd1-boot"), picks, the debug services, the receiver chosen on the speaker, the MCU's version, the boot slot, the updater, the receivers and their state; the native dialect's speaker snapshot, keys, volume requests and protocol version (`docs/protocol/registers.md`, generated from `protocol/spi`; `sequences.md`, Our OS). Our OS answers only our MCU firmware: none of the stock MCU's checks ("9112", the network-settings backup, the Cne defaults) |
| any id via the module shell `reg add <id> <name> <size>` | adds a RAM-only register served to the MCU on reads; `reg write <id> int16 <n>` sets it; gone after a module reboot. A patched stock MCU app once polled such a register every ~2 s through the stock read task, using its shared read slot (RAM 0x3ed4+0x10) only while idle [HW] |

Module shell view of the same file: `reg print [id]` (ids in decimal), `reg rd`/`wr`. Avoid printing 95-102 (WEP keys,
cipher, **WPA passphrase**, SSID).

## 7. Open questions
1. DRC 0xE1/0xE2/0x9E exact meaning and the unused codes 0xB2/0x13/0xFA (module `RemoteRegDRCCommand::passResult`).
2. The `03 01 x {00,03}` ↔ `03 01 01 {02,04}` exchange on 0x40 (and the unreachable `03 01 01 01`).
3. Module meanings of 0x11 = 0x20, 0x22, 0x24-0x26, and 0x6A result codes '1'-'6'.
4. Why 0x11 = 0x27 (self-DFU request) is honoured only on board variant 2.
5. Site-survey (0x68) request value; 0xDC special case in the read task (0x8000c424).
