# module/link: the MCU link, module side

`zd1-link`, a `no_std` Rust library without allocation: the module's side of the SPI register protocol, on the
protocol spec (`protocol/spi`, zd1-spi: registers, codes, framing), so the stock MCU firmware, ours and the MCU's DFU
bootloader talk to a module that runs our code instead of KnOS (`docs/protocol/spi-registers.md`). The behaviour on
top of the spec: reg 0x11 status (0x2100 = ready, or what the caller sets: the boot stub's 0x2000), reg 0x0C pending
(the status until read, then the queued notifications, oldest first), every other read the last value set or
written, writes handed out as events, HCR 0x28 = 0x21 = reboot into BL2 (mailbox + watchdog), the request line
(DM870 GPIO 6) high while anything is pending.

- `src/lib.rs`: `Link`, the protocol state machine (requests, notifications, values, the request line).
- The registers it uses (SPI slave, GPIO, the timestamp, the BL2 mailbox, the watchdog) come from `module/dm870` over
  its `Mmio` trait: a board only maps physical addresses (identity on bare metal, `/dev/mem` windows under Linux).
- `src/tests.rs`: the protocol against a fake SPI slave, including the stock MCU's drain replayed with the stock read
  kinds.

Used by `module/boot` (bare metal: the Linux boot stub, the mini test image) and `module/os/apps/zd1-linkd` (under
Linux).
```
cargo test -p zd1-link     # host tests (any Rust toolchain works; `nix develop` has one)
```
ARMv5TE has no atomics (no LDREX): keep dependencies that need `AtomicUsize` out.
