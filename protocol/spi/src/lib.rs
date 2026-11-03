//! The SPI register protocol between the Zeppelin Air's MCU (AT32UC3A) and its network module (DM870), as one
//! source of truth for both sides:
//! - [`spec`]: what the protocol is (registers, their kind and direction, DRC codes and their acks, status values),
//!   from the stock firmware (evidence noted per item, docs/protocol/spi-registers.md);
//! - [`framing`]: how it goes over the wire (byte stuffing, requests, replies), the one implementation;
//! - [`codec`]: what the payloads mean (volume, play state, network status).
//!
//! The module side (module/link, zd1-linkd) uses this crate directly; the MCU side (C) gets `mcu/src/zd1link_gen.h`
//! generated from it (`protocol/gen`), with the register reference and the test vectors both sides check against.
//! The MCU's other link, to a host computer over USB, is `zd1-hostlink` (protocol/host).
#![no_std]

pub mod codec;
pub mod framing;
pub mod spec;

#[cfg(test)]
mod tests;
