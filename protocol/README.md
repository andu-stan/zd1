# protocol: the MCU's two links, one definition each

Two `no_std` crates, one per link. Everything that speaks a link is built from its crate: Rust uses the crate
directly; the MCU's C (AVR32: no Rust there) is generated from it by `gen/`, with test vectors that hold the C to it.

| Crate | Link | Rust that uses it | Generated (`gen/`) |
|---|---|---|---|
| `spi/` (`zd1-spi`) | the MCU <-> the network module, SPI registers | `module/link` (on it `module/boot`, `zd1-linkd`) | `mcu/src/zd1link_gen.h` + its test, `docs/protocol/registers.md` |
| `host/` (`zd1-hostlink`) | a computer <-> the MCU over USB (the host link) | `host/core` (the `zd1` command line, the web page) | `mcu/src/zd1host_gen.h` + its test, `docs/protocol/host.md`, `host/spec.lock` |

The DFU bootloader's protocol (B&W's, on the same USB port) is not ours to define: `host/core/src/dfu.rs` speaks it,
checked frame by frame against the decompiled updater.

## spi/
| `src/` | |
|---|---|
| `spec.rs` | the registers (number, F/V kind, who reads / writes / notifies, payload, evidence) as a `registers!` table; DRC codes and their acks (`codes!`); the protocol's named values in groups (`consts!`: the wire's bytes, status, volume, the play XML's pattern and icons, receivers, boot slots, the native protocol). Checked against the stock MCU firmware's own kind table |
| `framing.rs` | the wire format: every byte stuffed (headers too), idle 0xFF, the module's request parser and reply encoders, the MCU's request encoders and reply parser |
| `codec.rs` | payloads: volume (attenuation <-> the MCU's 72 steps <-> %), play-state XML and its matcher, the configuration channel, network status |

The MCU's side is C: `mcu/src/zd1link_gen.h` has the values (generated from the three tables); `mcu/src/zd1link.h`
is the code on them, written by hand since the MCU can't run the Rust (stuffing, the volume conversion, net status,
the play-icon matcher), held to `framing.rs` and `codec.rs` by the generated test: every byte value, every volume
value both ways, so the C can't drift without the test failing.

## host/
`src/lib.rs` is the spec: the transport (control transfers to a vendor interface; its header comment says why), the
protocol version, and two declarations, `enums! { ... }` and `messages! { ... }` (`src/macros.rs`). Each item is written
once, with its `///` doc, and the declaration makes both:
- the Rust types: an enum per enum (`Source::Usb`, with `Unlisted(u8)` for a newer peer's value), a struct per message
  under `msg` (`msg::Select { source, player }`), requests tied to their replies (`codec::Request`);
- the table `ENUMS` / `MESSAGES` (ids, fields and their types and maximums, gated, timeouts, docs) that `gen` makes the
  C and the reference from, and that the codec reads.

`src/codec.rs` is the codec, table-driven: the one encoder and decoder on the computer's side, and the reference the
generated C is checked against. The typed messages go through it (`codec::encode_msg`, `Typed::from_msg`).

Values the two links share (the boot stub's slot, the receiver with the speaker) are the SPI spec's; the host spec
refers to them.

## Generated (`gen/`, zd1-protogen)
```
cargo run --manifest-path protocol/gen/Cargo.toml             # rewrite the generated files
cargo run --manifest-path protocol/gen/Cargo.toml -- --check  # fail if one is stale
cc -I mcu/src -o /tmp/t mcu/test/zd1link_gen_test.c && /tmp/t   # the SPI protocol's C against Rust's vectors
cc -I mcu/src -o /tmp/h mcu/test/zd1host_gen_test.c && /tmp/h   # the host link's C codec against the codec's
```
(any host Rust and C compiler; `nix develop` has them). `nix flake check` compiles and runs both tests
(`checks.mcu-tests`), and runs the crates' tests and `--check` (`checks.host-tests`).

| Output | |
|---|---|
| `mcu/src/zd1link_gen.h` | `ZD1_REG_*`, `zd1_reg_kind()`, `ZD1_DRC_*` + `zd1_drc_ack()`, every `consts!` group as `ZD1_<GROUP>_<NAME>`: under `mcu/src/zd1link.h` |
| `mcu/test/zd1link_gen_test.c` | the MCU's C (`zd1link.h` on `zd1link_gen.h`) against vectors computed by the Rust implementation (every byte stuffed, whole messages, every volume value both ways, kinds, acks, play icons) |
| `docs/protocol/registers.md` | the register and DRC reference |
| `mcu/src/zd1host_gen.h` | the host link's messages as C structs, a table-driven codec (header-only, `static inline`), the constants: used by `mcu/src/hostif.c` |
| `mcu/test/zd1host_gen_test.c` | the C codec against vectors from the codec: every message three ways (once if it has no fields), refused when short, long or over a maximum |
| `docs/protocol/host.md` | the host link's reference |
| `protocol/host/spec.lock` | every message and field the host link has had: what `gen` checks compatibility against |

## Tests
`cargo test -p zd1-spi -p zd1-hostlink -p zd1-link`: the SPI spec against the stock kind table, framing round trips,
codecs; the host link's spec is consistent (ids, replies, sizes, enums), every message's type matches its table entry,
the codec's compatibility rules; `module/link`'s behaviour, including the stock MCU's drain replayed with the stock
read kinds (every notification once, no loop).

## Zero cost (the SPI protocol, measured against the hand-written code before the crate)
- **C: header-only, `static inline`.** The compiler folds each helper into its call site; unused ones (the F/V table
  is a 32-byte bitmap behind `zd1_reg_kind()`) cost nothing.
- **Rust: one sink type per program.** The encoders are generic over `framing::Sink`; every closure is a type of its
  own, so closures at several call sites would compile the encoders several times. `module/link` writes into one
  `Tx` buffer type everywhere, and replies are encoded straight from where the data is (no staging copies).
- **`stuff()` is `#[inline(never)]`**: one shared copy, called from every encoder; at `opt-level = "s"` an inlined
  copy per encoder loop is bigger than the call. Branches pick a data source and share one encoder call site.
- Result: the bare-metal images (`module/boot`) are 84 bytes smaller than before; zd1-linkd is unchanged.

## Changing a protocol
SPI: edit `spi/src/spec.rs` (with the evidence; a new value goes into a `consts!` group, and the C has it), regenerate,
run both test sets; try it end to end in the emulator with
both sides ours (`python3 emu/zemu run --mcu`, `--mcu stock` for the stock MCU firmware), then on the unit.

The host link only grows (`docs/protocol/host.md`, Compatibility): new requests with new ids, new fields at the end
of a message, nothing reused or retyped. Add it to `host/src/lib.rs`, regenerate: `host/spec.lock` records every
message and field there has been, and `gen` refuses a change that breaks the rules (a real break: bump `VERSION`,
run `gen -- --break`; the tools refuse firmware with another version). Mark a request `gated` if it changes the
speaker or reads its RAM. Then the handler in `mcu/src/hostif.c`, and what uses it in `host/core` or `host/cli`; in
the emulator `zemu run --mcu --host 'zd1 ...'` (the gate is open there).
