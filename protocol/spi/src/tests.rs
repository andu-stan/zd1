extern crate std;
use crate::framing::*;
use crate::spec::*;
use std::vec::Vec;

fn bytes(f: impl FnOnce(&mut dyn FnMut(u8))) -> Vec<u8> {
    let mut v = Vec::new();
    f(&mut |b| v.push(b));
    v
}

#[test]
fn spec_agrees_with_the_stock_kind_table() {
    for r in REGISTERS {
        let stock = if STOCK_V.contains(&r.num) {
            Some(Kind::V)
        } else if STOCK_F.contains(&r.num) {
            Some(Kind::F)
        } else {
            None
        };
        if let Some(k) = stock {
            assert_eq!(k, r.kind, "{} 0x{:02x}", r.name, r.num);
        }
    }
    for w in REGISTERS.windows(2) {
        assert!(w[0].num < w[1].num, "{} / {}: sorted, unique", w[0].name, w[1].name);
    }
}

#[test]
fn drc_acks() {
    assert_eq!(Drc::from_code(0xDF), Some(Drc::Play));
    assert_eq!(Drc::Play.ack(), Some(0));
    assert_eq!(Drc::Enable.ack(), Some(5));
    assert_eq!(Drc::PlayPause.ack(), None);
}

#[test]
fn stuffing_covers_headers() {
    // V read of 0x27 (VersionCNE): the stock MCU unstuffs every byte, so the register byte is stuffed too
    assert_eq!(bytes(|o| request(READ_V, 0x27, 0, 0, &mut |b| o(b))), [0x03, 0x27, 0x27, 0x00, 0x00]);
    assert_eq!(bytes(|o| reply_v(0x27, b"C\xff", &mut |b| o(b))), [0x03, 0x27, 0x27, 2, 0, b'C', 0x27, 0x01]);
}

#[test]
fn request_parser_round_trip_and_idle() {
    let mut p = RequestParser::<16>::default();
    let mut got = Vec::new();
    let mut wire = bytes(|o| request(READ_F, 0x27, 0, 0, &mut |b| o(b)));
    wire.insert(1, IDLE); // idle clocking anywhere is ignored
    wire.extend(bytes(|o| write_v(0x40, &[3, 3, 1, 0xFF], &mut |b| o(b))));
    for b in wire {
        if let Some(r) = p.push(b) {
            got.push(match r {
                Request::ReadF { reg } => (1, reg, std::vec![]),
                Request::WriteV { reg, data, .. } => (2, reg, data.to_vec()),
                _ => (9, 0, std::vec![]),
            });
        }
    }
    assert_eq!(got, [(1, 0x27, std::vec![]), (2, 0x40, std::vec![3, 3, 1, 0xFF])]);
}

#[test]
fn reply_parser_reads_what_the_module_sends() {
    let mut r = ReplyParser::<32>::new(READ_V, 0x27);
    let mut wire = std::vec![IDLE, IDLE];
    wire.extend(bytes(|o| reply_v(0x27, b"C 2014", &mut |b| o(b))));
    let mut out = Reply::Pending;
    for b in wire {
        out = r.push(b);
        if out != Reply::Pending {
            break;
        }
    }
    assert_eq!(out, Reply::V(6));
    assert_eq!(&r.data[..6], b"C 2014");
    let mut r = ReplyParser::<4>::new(READ_F, 0x32);
    let mut out = Reply::Pending;
    for b in bytes(|o| reply_f(READ_F, 0x32, 0x3231, &mut |b| o(b))) {
        out = r.push(b);
    }
    assert_eq!(out, Reply::F(0x3231));
}

#[test]
fn volume_mapping_matches_the_mcu() {
    use crate::codec::volume::*;
    assert_eq!(step_from_att(0), 71);
    assert_eq!(step_from_att(MUTE), 0);
    assert_eq!(att_from_percent(50), 0x78);
    assert_eq!(step_from_att(0x78), 36); // os/control: 50 % -> 0x78 -> step 36
    assert_eq!(percent_from_att(att_from_step(37)), 52); // os/control: step 37 -> 52 %
}

#[test]
fn play_icon_split_across_pieces() {
    use crate::codec::play::*;
    let mut m = Matcher::default();
    let mut found = None;
    let mut msg = Vec::new();
    xml(Play::Playing, &mut |b| msg.extend_from_slice(b));
    for piece in msg.chunks(5) {
        for &c in piece {
            if let Some(p) = m.push(c) {
                found = Some(p);
            }
        }
    }
    assert_eq!(found, Some(Play::Playing));
}

#[test]
fn native_volume_shares_are_exact_for_every_step() {
    use crate::spec::native::{percent_from_step, step_from_percent};
    for cap in 1..=71u8 {
        for step in 0..=cap {
            assert_eq!(step_from_percent(percent_from_step(step, cap), cap), step, "cap {cap} step {step}");
        }
        assert_eq!(percent_from_step(cap + 5, cap), 100);
    }
}
