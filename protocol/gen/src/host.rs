//! The host link (zd1-hostlink): the MCU's C, its conformance test and the reference. The reference encoder is the
//! host side's codec (zd1-hostlink `codec`, which host/core runs): test vectors computed by it are baked into the C
//! test.

use std::fmt::Write as _;
use zd1_hostlink::{
    self as host, ENUMS, MESSAGES, Message, Ty,
    codec::{self, Val},
};

use crate::{HOST_HEADER as HEADER, c_comment, screaming};

/// The reference encoder: the codec's (zd1-hostlink, `codec`), which the host side runs.
pub fn encode(m: &'static Message, vals: &[Val]) -> Vec<u8> {
    let v = codec::encode_vals(m, vals).unwrap_or_else(|e| panic!("{e}"));
    assert!(v.len() <= host::MSG_MAX);
    v
}

/// Sample values: variant 0 typical and short (strings with UTF-8), 1 the largest (every length at its maximum, top
/// values), 2 empty strings and zeros.
pub fn sample(m: &Message, variant: usize) -> Vec<Val> {
    m.fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let k = (m.id as u32) * 31 + i as u32 * 7 + 1;
            match (f.ty, variant) {
                (_, 2) if !matches!(f.ty, Ty::Str(_) | Ty::Bytes(_)) => {
                    if f.ty == Ty::Bool {
                        Val::B(false)
                    } else {
                        Val::N(0)
                    }
                }
                (Ty::Bool, _) => Val::B(variant == 1 || k % 2 == 1),
                (Ty::U8, 1) => Val::N(0xFF),
                (Ty::U16, 1) => Val::N(0xFFFE),
                (Ty::U32, 1) => Val::N(0xFFFF_FFFD),
                (Ty::U8, _) => Val::N(k & 0xFF),
                (Ty::U16, _) => Val::N((k * 0x123) & 0xFFFF),
                (Ty::U32, _) => Val::N(k.wrapping_mul(0x0101_0453)),
                (Ty::Enum(e), _) => {
                    let vs = host::enum_(e).unwrap().values;
                    Val::N(vs[(k as usize + variant) % vs.len()].1 as u32)
                }
                (Ty::Str(max), v) => Val::S(match v {
                    1 => (0..max).map(|j| b'A' + (j % 26)).collect(),
                    2 => vec![],
                    _ => format!("{} {}é", f.name, k).into_bytes().into_iter().take(max as usize).collect(),
                }),
                (Ty::Bytes(max), v) => Val::S(match v {
                    1 => (0..max).map(|j| (j as u8).wrapping_mul(37) ^ 0x5A).collect(),
                    2 => vec![],
                    _ => (0..(k % 9 + 3)).map(|j| (j * 41 + k) as u8).collect(),
                }),
            }
        })
        .collect()
}

fn snake(camel: &str) -> String {
    screaming(camel).to_ascii_lowercase()
}

fn c_ty(t: Ty) -> &'static str {
    match t {
        Ty::U8 | Ty::Enum(_) => "uint8_t",
        Ty::U16 => "uint16_t",
        Ty::U32 => "uint32_t",
        Ty::Bool => "bool",
        Ty::Str(_) => "char",
        Ty::Bytes(_) => "uint8_t",
    }
}

pub fn header_h() -> String {
    let mut s = format!(
        "/* {HEADER}\n * The host <-> MCU protocol over USB (docs/protocol/host.md, protocol/host/src/lib.rs). */\n#ifndef ZD1HOST_GEN_H\n#define ZD1HOST_GEN_H\n#include <stdbool.h>\n#include <stddef.h>\n#include <stdint.h>\n#include <string.h>\n\n"
    );
    writeln!(s, "#define ZD1H_VERSION {}", host::VERSION).unwrap();
    writeln!(s, "#define ZD1H_USB_IFACE    {}\n#define ZD1H_USB_CLASS    0x{:02X}\n#define ZD1H_USB_SUBCLASS 0x{:02X}\n#define ZD1H_USB_PROTOCOL 0x{:02X}",
             host::usb::IFACE, host::usb::CLASS, host::usb::SUBCLASS, host::usb::PROTOCOL).unwrap();
    writeln!(s, "#define ZD1H_REQ_CALL  0x{:02X}  /* OUT: wValue = sequence number, data = a request */\n#define ZD1H_REQ_POLL  0x{:02X}  /* IN: records */\n#define ZD1H_REQ_HELLO 0x{:02X}  /* IN: HelloReply, from the interrupt */",
             host::usb::CALL, host::usb::POLL, host::usb::HELLO).unwrap();
    writeln!(
        s,
        "#define ZD1H_USB_BULK_OUT 0x{:02X}  /* the bulk OUT endpoint: the Pipe's data */",
        host::usb::BULK_OUT
    )
    .unwrap();
    writeln!(s, "#define ZD1H_MSG_MAX  {}\n#define ZD1H_POLL_MAX {}\n#define ZD1H_REC_TEXT  {}  /* log text */\n#define ZD1H_REC_REPLY {}  /* u16 seq + a message */\n#define ZD1H_REC_CONSOLE {}  /* the module's console, raw */",
             host::MSG_MAX, host::POLL_MAX, host::record::TEXT, host::record::REPLY, host::record::CONSOLE).unwrap();
    s += "\n/* ---- enums ---- */\n";
    for e in ENUMS {
        writeln!(s, "/* {}: {} */", e.name, c_comment(e.doc)).unwrap();
        for (n, v, d) in e.values {
            writeln!(s, "#define ZD1H_{}_{:<10} {v:>3}  /* {} */", screaming(e.name), n.to_uppercase(), c_comment(d))
                .unwrap();
        }
    }
    s += "\n/* ---- message ids ---- */\n";
    for m in MESSAGES.iter().filter(|m| !m.module) {
        writeln!(
            s,
            "#define ZD1H_ID_{:<14} 0x{:02X}{}",
            screaming(m.name),
            m.id,
            if m.gated { "  /* gated */" } else { "" }
        )
        .unwrap();
    }
    let bits = |pick: &dyn Fn(&Message) -> bool| {
        let mut b = [0u8; 32];
        for m in MESSAGES.iter().filter(|m| pick(m)) {
            b[m.id as usize / 8] |= 1 << (m.id % 8);
        }
        b.iter().map(|x| format!("0x{x:02X}")).collect::<Vec<_>>().join(",")
    };
    writeln!(s, "\n/* the requests (HelloReply.requests: bit id % 8 of byte id / 8; the emulator's build has more), and the gated ones */\n#ifdef ZD1_EMU\nstatic const uint8_t zd1h_requests[32] = {{{}}};\n#else\nstatic const uint8_t zd1h_requests[32] = {{{}}};\n#endif\nstatic inline bool zd1h_gated(uint8_t id){{\n    static const uint8_t g[32] = {{{}}};\n    return (g[id >> 3] >> (id & 7)) & 1;\n}}",
             bits(&|m| m.reply.is_some()), bits(&|m| m.reply.is_some() && !m.emu), bits(&|m| m.gated)).unwrap();
    s += "\n/* ---- the largest bytes fields, and the bits of the masks (which fields a request changes) ---- */\n";
    for m in MESSAGES.iter().filter(|m| !m.module) {
        for f in m.fields {
            if let Ty::Bytes(max) = f.ty {
                writeln!(s, "#define ZD1H_{}_{}_MAX {max}", screaming(m.name), f.name.to_uppercase()).unwrap();
            }
        }
        if let Some(mask) = m.fields.iter().find(|f| f.mask) {
            for f in m.fields {
                if let Some(b) = m.mask_bit(f.name) {
                    writeln!(
                        s,
                        "#define ZD1H_{}_{}_{} 0x{b:02X}u",
                        screaming(m.name),
                        mask.name.to_uppercase(),
                        f.name.to_uppercase()
                    )
                    .unwrap();
                }
            }
        }
    }
    s += "\n/* ---- messages: one struct each (str: NUL-terminated; bytes: + NAME_len); the module's records are not the MCU's ---- */\n";
    for m in MESSAGES.iter().filter(|m| !m.module) {
        writeln!(
            s,
            "/* {}{} */",
            m.name,
            if m.doc.is_empty() { String::new() } else { format!(": {}", c_comment(m.doc)) }
        )
        .unwrap();
        s += "typedef struct {";
        if m.fields.is_empty() {
            s += " uint8_t unused_;";
        }
        for f in m.fields {
            match f.ty {
                Ty::Str(max) => write!(s, " char {}[{}];", f.name, max as usize + 1).unwrap(),
                Ty::Bytes(max) => write!(s, " uint16_t {0}_len; uint8_t {0}[{max}];", f.name).unwrap(),
                t => write!(s, " {} {};", c_ty(t), f.name).unwrap(),
            }
        }
        writeln!(s, " }} zd1h_{}_t;", snake(m.name)).unwrap();
    }
    s += CODEC_C;
    s += "\n/* ---- the field tables ---- */\n";
    for m in MESSAGES.iter().filter(|m| !m.module) {
        let t = format!("zd1h_{}_t", snake(m.name));
        if m.fields.is_empty() {
            writeln!(
                s,
                "static const zd1h_msg_t ZD1H_{} = {{ 0x{:02X}, 0, 0, sizeof({t}) }};",
                screaming(m.name),
                m.id
            )
            .unwrap();
            continue;
        }
        write!(s, "static const zd1h_field_t zd1h_{}_fields[] = {{", snake(m.name)).unwrap();
        for f in m.fields {
            let (ty, max, len_off) = match f.ty {
                Ty::U8 | Ty::Enum(_) => ("ZD1H_T_U8", 0, "0".to_string()),
                Ty::Bool => ("ZD1H_T_BOOL", 0, "0".into()),
                Ty::U16 => ("ZD1H_T_U16", 0, "0".into()),
                Ty::U32 => ("ZD1H_T_U32", 0, "0".into()),
                Ty::Str(m) => ("ZD1H_T_STR", m as u16, "0".into()),
                Ty::Bytes(m) => ("ZD1H_T_BYTES", m, format!("offsetof({t}, {}_len)", f.name)),
            };
            write!(s, " {{ {ty}, {max}, offsetof({t}, {}), {len_off} }},", f.name).unwrap();
        }
        s.pop();
        writeln!(
            s,
            " }};\nstatic const zd1h_msg_t ZD1H_{} = {{ 0x{:02X}, {}, zd1h_{}_fields, sizeof({t}) }};",
            screaming(m.name),
            m.id,
            m.fields.len(),
            snake(m.name)
        )
        .unwrap();
    }
    s += "\n#endif\n";
    s
}

/// The codec: table-driven, static inline (only what a program uses is compiled in). Struct fields are read and
/// written as their own C types, so it works on the big-endian MCU and a little-endian host alike.
const CODEC_C: &str = r#"
/* ---- codec ---- */
enum { ZD1H_T_U8, ZD1H_T_U16, ZD1H_T_U32, ZD1H_T_BOOL, ZD1H_T_STR, ZD1H_T_BYTES };
typedef struct { uint8_t type; uint16_t max; uint16_t off; uint16_t len_off; } zd1h_field_t;
typedef struct { uint8_t id; uint8_t n; const zd1h_field_t *f; uint16_t size; } zd1h_msg_t;

/* the struct `s` as message `m` into out[cap]: the length, or -1 (doesn't fit, a length over its maximum) */
static inline int zd1h_encode(const zd1h_msg_t *m, const void *s, uint8_t *out, int cap){
    const uint8_t *p = (const uint8_t *)s;
    int n = 0;
    if(cap < 1) return -1;
    out[n++] = m->id;
    for(int i = 0; i < m->n; i++){
        const zd1h_field_t *f = &m->f[i];
        const uint8_t *v = p + f->off;
        uint32_t x = 0, l = 0;
        switch(f->type){
        case ZD1H_T_U8:   x = *v; l = 1; break;
        case ZD1H_T_BOOL: x = *(const bool *)v ? 1 : 0; l = 1; break;
        case ZD1H_T_U16:  x = *(const uint16_t *)v; l = 2; break;
        case ZD1H_T_U32:  x = *(const uint32_t *)v; l = 4; break;
        case ZD1H_T_STR:
            while(l < f->max && v[l]) l++;
            if(n + 1 + (int)l > cap) return -1;
            out[n++] = (uint8_t)l; memcpy(out + n, v, l); n += l;
            continue;
        default:          /* ZD1H_T_BYTES */
            l = *(const uint16_t *)(p + f->len_off);
            if(l > f->max || n + 2 + (int)l > cap) return -1;
            out[n++] = (uint8_t)l; out[n++] = (uint8_t)(l >> 8); memcpy(out + n, v, l); n += l;
            continue;
        }
        if(n + (int)l > cap) return -1;
        for(uint32_t k = 0; k < l; k++) out[n++] = (uint8_t)(x >> (8 * k));
    }
    return n;
}

/* message `m` from in[len] into the struct `s`: 0, or -1 (another id, a field cut short, a length over its
 * maximum). The compatibility rules (protocol/host/src/lib.rs): fields after the last one sent stay zero / empty (an
 * older peer), bytes after the fields we know are ignored (a newer peer's). */
static inline int zd1h_decode(const zd1h_msg_t *m, const uint8_t *in, int len, void *s){
    uint8_t *p = (uint8_t *)s;
    int k = 1;
    if(len < 1 || in[0] != m->id) return -1;
    memset(s, 0, m->size);
    for(int i = 0; i < m->n && k < len; i++){
        const zd1h_field_t *f = &m->f[i];
        uint8_t *v = p + f->off;
        int need = f->type == ZD1H_T_U16 ? 2 : f->type == ZD1H_T_U32 ? 4 : f->type == ZD1H_T_BYTES ? 2 : 1;
        if(k + need > len) return -1;
        uint32_t x = 0;
        for(int b = 0; b < need; b++) x |= (uint32_t)in[k + b] << (8 * b);
        k += need;
        switch(f->type){
        case ZD1H_T_U8:   *v = (uint8_t)x; break;
        case ZD1H_T_BOOL: if(x > 1) return -1; *(bool *)v = x != 0; break;
        case ZD1H_T_U16:  *(uint16_t *)v = (uint16_t)x; break;
        case ZD1H_T_U32:  *(uint32_t *)v = x; break;
        case ZD1H_T_STR:
            if(x > f->max || k + (int)x > len) return -1;
            memcpy(v, in + k, x); v[x] = 0; k += x;
            break;
        default:          /* ZD1H_T_BYTES */
            if(x > f->max || k + (int)x > len) return -1;
            memcpy(v, in + k, x); *(uint16_t *)(p + f->len_off) = (uint16_t)x; k += x;
            break;
        }
    }
    return 0;
}
"#;

fn c_bytes(v: &[u8]) -> String {
    if v.is_empty() {
        return "0".into();
    }
    v.iter().map(|b| format!("0x{b:02X}")).collect::<Vec<_>>().join(",")
}

fn c_str(s: &[u8]) -> String {
    let mut o = String::from("\"");
    for &b in s {
        if b.is_ascii_alphanumeric() || b == b' ' { o.push(b as char) } else { write!(o, "\\x{b:02x}\"\"").unwrap() }
    }
    o + "\""
}

/// The C codec against the reference encoder: each message's sample values encoded by C must be the vector, and the
/// vector decoded by C must give the values back; with bytes after it the same (a newer peer), without its last field
/// that one zero (an older peer); a field cut short, another id or a length over its maximum must be refused.
pub fn test_c() -> String {
    let mut s = format!(
        "/* {HEADER}\n * Host test of mcu/src/zd1host_gen.h against the reference encoder (protocol/gen):\n *   cc -I mcu/src -o /tmp/t mcu/test/zd1host_gen_test.c && /tmp/t */\n#include <stdio.h>\n#include \"zd1host_gen.h\"\nstatic int fails;\n#define CHECK(c, ...) do {{ if(!(c)){{ fails++; printf(\"FAIL \" __VA_ARGS__); printf(\"\\n\"); }} }} while(0)\n\n"
    );
    let mut calls = Vec::new();
    for m in MESSAGES.iter().filter(|m| !m.module) {
        let t = format!("zd1h_{}_t", snake(m.name));
        let mut seen = Vec::new();
        for variant in 0..3 {
            let vals = sample(m, variant);
            let wire = encode(m, &vals);
            if seen.contains(&wire) {
                continue; // the same bytes as an earlier variant (a message without fields): no second case
            }
            seen.push(wire.clone());
            let fname = format!("{}_{variant}", snake(m.name));
            calls.push(fname.clone());
            writeln!(s, "static void {fname}(void){{\n    static const uint8_t w[] = {{{}}};\n    {t} a, b; uint8_t out[ZD1H_MSG_MAX]; int n;\n    memset(&a, 0, sizeof a); memset(&b, 0xA5, sizeof b);",
                     c_bytes(&wire)).unwrap();
            let mut eq = Vec::new();
            for (f, v) in m.fields.iter().zip(&vals) {
                match (f.ty, v) {
                    (Ty::Str(_), Val::S(x)) => {
                        writeln!(s, "    memcpy(a.{0}, {1}, {2}); a.{0}[{2}] = 0;", f.name, c_str(x), x.len()).unwrap();
                        eq.push(format!("!strcmp(a.{0}, b.{0})", f.name));
                    }
                    (Ty::Bytes(_), Val::S(x)) => {
                        writeln!(
                            s,
                            "    {{ static const uint8_t d[] = {{{}}}; memcpy(a.{1}, d, {2}); a.{1}_len = {2}; }}",
                            c_bytes(x),
                            f.name,
                            x.len()
                        )
                        .unwrap();
                        eq.push(format!("a.{0}_len == b.{0}_len && !memcmp(a.{0}, b.{0}, a.{0}_len)", f.name));
                    }
                    (_, Val::B(x)) => {
                        writeln!(s, "    a.{} = {x};", f.name).unwrap();
                        eq.push(format!("a.{0} == b.{0}", f.name));
                    }
                    (_, Val::N(x)) => {
                        writeln!(s, "    a.{} = {x}u;", f.name).unwrap();
                        eq.push(format!("a.{0} == b.{0}", f.name));
                    }
                    _ => unreachable!(),
                }
            }
            let msg = format!("ZD1H_{}", screaming(m.name));
            writeln!(s, "    n = zd1h_encode(&{msg}, &a, out, sizeof out);\n    CHECK(n == (int)sizeof w && !memcmp(out, w, n), \"{fname}: encode\");\n    CHECK(zd1h_decode(&{msg}, w, sizeof w, &b) == 0, \"{fname}: decode\");").unwrap();
            if !eq.is_empty() {
                writeln!(s, "    CHECK({}, \"{fname}: fields\");", eq.join(" && ")).unwrap();
            }
            // a newer peer's message: bytes after our fields are ignored
            writeln!(s, "    {{ uint8_t x[sizeof w + 3]; memcpy(x, w, sizeof w); x[sizeof w] = 7; x[sizeof w + 1] = 0xFF; x[sizeof w + 2] = 0; memset(&b, 0xA5, sizeof b); CHECK(zd1h_decode(&{msg}, x, sizeof x, &b) == 0{}, \"{fname}: trailing bytes\"); }}",
                     if eq.is_empty() { String::new() } else { format!(" && {}", eq.join(" && ")) }).unwrap();
            writeln!(s, "    CHECK(zd1h_encode(&{msg}, &a, out, sizeof w - 1) < 0, \"{fname}: no room\");\n    {{ uint8_t x[sizeof w]; memcpy(x, w, sizeof w); x[0] ^= 0x80; CHECK(zd1h_decode(&{msg}, x, sizeof x, &b) < 0, \"{fname}: another id\"); }}").unwrap();
            if let Some((last, rest)) = m.fields.split_last() {
                // an older peer's message: it ends before the last field, which stays zero / empty
                let cut = 1 + rest.iter().zip(&vals).map(|(f, v)| field_len(f.ty, v)).sum::<usize>();
                let zero = match last.ty {
                    Ty::Str(_) => format!("b.{}[0] == 0", last.name),
                    Ty::Bytes(_) => format!("b.{}_len == 0", last.name),
                    _ => format!("!b.{}", last.name),
                };
                let keep: Vec<&String> = eq[..rest.len()].iter().collect();
                writeln!(s, "    memset(&b, 0xA5, sizeof b); CHECK(zd1h_decode(&{msg}, w, {cut}, &b) == 0 && {zero}{}, \"{fname}: without its last field\");",
                         keep.iter().map(|e| format!(" && {e}")).collect::<String>()).unwrap();
            }
            // a field cut in the middle: refused
            let mut at = 1;
            for (f, v) in m.fields.iter().zip(&vals) {
                let l = field_len(f.ty, v);
                if l >= 2 {
                    writeln!(s, "    CHECK(zd1h_decode(&{msg}, w, {}, &b) < 0, \"{fname}: {} cut\");", at + 1, f.name)
                        .unwrap();
                    break;
                }
                at += l;
            }
            // a string or byte field one over its maximum: refused
            if variant == 1
                && let Some(i) =
                    m.fields.iter().position(|f| matches!(f.ty, Ty::Str(m) if m < 255) || matches!(f.ty, Ty::Bytes(_)))
            {
                let f = &m.fields[i];
                // the fields before it as encoded, then it one byte longer than its maximum
                let mut bad = wire
                    [..1 + m.fields[..i].iter().zip(&vals).map(|(f, v)| field_len(f.ty, v)).sum::<usize>()]
                    .to_vec();
                let Val::S(x) = &vals[i] else { unreachable!() };
                let x = [x.as_slice(), b"!"].concat();
                match f.ty {
                    Ty::Str(_) => bad.push(x.len() as u8),
                    _ => bad.extend((x.len() as u16).to_le_bytes()),
                }
                bad.extend(&x);
                writeln!(s, "    {{ static const uint8_t x[] = {{{}}}; CHECK(zd1h_decode(&{msg}, x, sizeof x, &b) < 0, \"{fname}: {} over its maximum\"); }}", c_bytes(&bad), f.name).unwrap();
            }
            s += "}\n";
        }
    }
    s += "\nint main(void){\n";
    for c in &calls {
        writeln!(s, "    {c}();").unwrap();
    }
    s += "    printf(fails ? \"zd1host_gen: %d FAILED\\n\" : \"zd1host_gen: all ok\\n\", fails);\n    return fails != 0;\n}\n";
    s
}

fn field_len(t: Ty, v: &Val) -> usize {
    match (t, v) {
        (Ty::Str(_), Val::S(x)) => 1 + x.len(),
        (Ty::Bytes(_), Val::S(x)) => 2 + x.len(),
        (t, _) => t.size().0,
    }
}

/// The reference: messages and enums, from the spec's own words.
pub fn docs() -> String {
    let mut s = format!(
        "# Host <-> MCU protocol (generated)\n\n<!-- {HEADER} -->\n\nHow a computer on the speaker's USB port talks to our MCU firmware: typed messages over control transfers to a\nvendor interface. Defined in `protocol/host/src/lib.rs` (its header comment explains the transport): the MCU's C\n(`mcu/src/zd1host_gen.h`) is generated from it; the computer's side uses the crate itself (`host/core`: the `zd1`\ncommand line and the web page).\n\nProtocol version {}. USB: {:04X}:{:04X}, the vendor interface of class {:02X}/{:02X}/{:02X} (interface {} in our firmware; hosts find\nit by its class, its number moves with the interfaces before it). Requests (vendor, to the interface): CALL 0x{:02X} (OUT, `wValue` = sequence number, one request), POLL 0x{:02X} (IN, up to {} bytes of\nrecords: kind, u16 length, payload; 1 = log text, 2 = reply = u16 sequence number + message, 3 = the module's console's\nbytes, while `Console` has it open), HELLO 0x{:02X} (IN, `HelloReply`, answered from the interrupt). Messages are at most {} bytes: id, then the fields in order,\nlittle-endian; `str` = u8 length + bytes, `bytes` = u16 length + bytes. Any request may be answered with `Error`.\n\n## Compatibility\n\nFirmware and tools of different ages meet, so the protocol only grows: an id is never reused, a message gains fields only\nat its end, a field never changes its type or maximum. A decoder ignores bytes after the fields it knows (a newer\npeer's) and leaves fields a shorter message doesn't reach at zero / false / empty (an older peer's); a field cut in the\nmiddle or a length over its maximum is an error. `HelloReply.requests` lists the firmware's requests; an unknown one\ngets `Error UNKNOWN`. `protocol/host/spec.lock` records every message and field there has been: `protocol/gen` refuses\na change that breaks these rules unless run with `--break`, which needs a new protocol version. Across versions the\nbase layer holds: HELLO (from the interrupt) and `Dfu` (id 0x06, unchanged since version 1), so a tool of any version\ncan put a firmware of any version into the bootloader and flash it; a tool refuses every other request to a firmware\nof another version.\n\n## The gate\n\nRequests marked gated change the speaker (or read its RAM). With the gate locked they get `Error LOCKED`; `Unlock`\nwaits up to 30 s for a press of ON/OFF on the speaker (its LED flashes purple; the press does nothing else), after\nwhich gated requests run until 10 minutes after the last one. A firmware's first start after it was flashed (its version\nisn't the one the EEPROM recorded) is unlocked the same way: whoever could flash it could change anything already, and\nan install goes on without a press. `Lock` closes it, or opens it for good (kept in the\nEEPROM, for a unit tools drive unattended). The device can't tell which program or web page is talking to it: the\npress is its own check. `zd1` and the web page ask for the press and unlock by themselves when a request needs it.\n\n",
        host::VERSION,
        host::usb::VID,
        host::usb::PID,
        host::usb::CLASS,
        host::usb::SUBCLASS,
        host::usb::PROTOCOL,
        host::usb::IFACE,
        host::usb::CALL,
        host::usb::POLL,
        host::POLL_MAX,
        host::usb::HELLO,
        host::MSG_MAX
    );
    s = s.replace(
        "\n\n## Compatibility",
        &format!(
            " Bulk data (`Pipe`) goes to the interface's one endpoint, bulk OUT \
        0x{:02X}, 64-byte packets; the MCU NAKs while its buffers are full.\n\n## Compatibility",
            host::usb::BULK_OUT
        ),
    );
    let ty = |t: Ty| match t {
        Ty::U8 => "u8".to_string(),
        Ty::U16 => "u16".into(),
        Ty::U32 => "u32".into(),
        Ty::Bool => "bool".into(),
        Ty::Enum(e) => format!("[{e}](#{})", e.to_lowercase()),
        Ty::Str(m) => format!("str <= {m}"),
        Ty::Bytes(m) => format!("bytes <= {m}"),
    };
    let fields = |m: &Message| {
        if m.fields.is_empty() {
            "-".to_string()
        } else {
            m.fields
                .iter()
                .map(|f| {
                    let bits = if f.mask {
                        let b: Vec<String> = m
                            .fields
                            .iter()
                            .filter_map(|g| Some(format!("0x{:02X} `{}`", m.mask_bit(g.name)?, g.name)))
                            .collect();
                        format!(" ({})", b.join(", "))
                    } else {
                        String::new()
                    };
                    format!(
                        "`{}` {}{}{bits}",
                        f.name,
                        ty(f.ty),
                        if f.doc.is_empty() { String::new() } else { format!(": {}", f.doc) }
                    )
                })
                .collect::<Vec<_>>()
                .join("<br>")
        }
    };
    s += "## Requests\n\nThe host's timeout is the request's own: the MCU answers within it, or the host gives up.\n\n| Id | Request | Gated | Fields | Reply | Timeout | |\n|---|---|---|---|---|---|---|\n";
    for m in MESSAGES.iter().filter(|m| m.reply.is_some()) {
        let timeout = match m.wait {
            Some((f, zero)) => format!("{} s + `{f}` s (0: {zero})", m.timeout_ms / 1000),
            None => format!("{} s", m.timeout_ms as f64 / 1000.0),
        };
        writeln!(
            s,
            "| 0x{:02X} | {} | {} | {} | {} | {timeout} | {}{} |",
            m.id,
            m.name,
            if m.gated { "gated" } else { "" },
            fields(m).replace('|', "\\|"),
            m.reply.unwrap(),
            m.doc.replace('|', "\\|"),
            if m.emu { " The emulator's build only." } else { "" }
        )
        .unwrap();
    }
    s += "\n## Replies\n\n| Id | Reply | Fields | |\n|---|---|---|---|\n";
    for m in MESSAGES.iter().filter(|m| m.reply.is_none() && !m.module) {
        writeln!(
            s,
            "| 0x{:02X} | {} | {} | {} |",
            m.id,
            m.name,
            fields(m).replace('|', "\\|"),
            m.doc.replace('|', "\\|")
        )
        .unwrap();
    }
    s += "\n## The module's records\n\nEncoded by the module OS with this codec, relayed by the MCU (`ModRecord`), decoded by the host: the MCU's C has \
none of them. A field marked set is a setting (docs/module/settings.md): the host writes it with `SetSetting`, the key \
the field's name with dots for underscores, the value in its string form (bools `on` / `off`, enums by name); the \
module checks it against the type, the maximum and the format.\n";
    for m in MESSAGES.iter().filter(|m| m.module) {
        writeln!(s, "\n### {} (0x{:02X})\n{}\n\n| Field | Type | Setting | |\n|---|---|---|---|", m.name, m.id, m.doc)
            .unwrap();
        for f in m.fields {
            let setting = if !f.set {
                String::new()
            } else {
                let mut x = format!("`{}`", f.key());
                if !f.format.is_empty() {
                    x += &format!(", {}", f.format);
                }
                if !f.default.is_empty() {
                    x += &format!(", default `{}`", f.default);
                }
                x
            };
            writeln!(s, "| `{}` | {} | {} | {} |", f.name, ty(f.ty), setting, f.doc.replace('|', "\\|")).unwrap();
        }
    }
    s += "\n## Enums\n";
    for e in ENUMS {
        writeln!(s, "\n### {}\n{}\n\n| Value | Name | |\n|---|---|---|", e.name, e.doc).unwrap();
        for (n, v, d) in e.values {
            writeln!(s, "| {v} | {} | {d} |", n.to_uppercase()).unwrap();
        }
    }
    s
}

/// spec.lock: every message and field the protocol has had, one line each; what decides compatibility (the kind of a
/// message, each field's type and maximum, each enum value) and, for a reader, the names.
pub fn lock_text() -> String {
    let mut s = format!(
        "# protocol/host/spec.lock: every message and field the host link has had (protocol/gen keeps it).\n# Lines may be added; a changed or missing one is a break (docs/protocol/host.md, Compatibility).\nversion {}\n",
        host::VERSION
    );
    for m in MESSAGES {
        writeln!(
            s,
            "msg 0x{:02X} {} {}",
            m.id,
            if m.module {
                "record"
            } else if m.reply.is_some() {
                "request"
            } else {
                "reply"
            },
            m.name
        )
        .unwrap();
        for (i, f) in m.fields.iter().enumerate() {
            let t = match f.ty {
                Ty::U8 => "u8".to_string(),
                Ty::U16 => "u16".into(),
                Ty::U32 => "u32".into(),
                Ty::Bool => "bool".into(),
                Ty::Enum(e) => format!("enum:{e}"),
                Ty::Str(m) => format!("str:{m}"),
                Ty::Bytes(m) => format!("bytes:{m}"),
            };
            writeln!(s, "field 0x{:02X} {i} {t} {}", m.id, f.name).unwrap();
        }
    }
    for e in ENUMS {
        for (n, v, _) in e.values {
            writeln!(s, "enum {} {v} {}", e.name, n.to_uppercase()).unwrap();
        }
    }
    s
}

/// What in the old lock the current spec breaks: a message or field gone or changed, an enum value gone, another
/// version. Names may change (they are not on the wire); lines may be added.
pub fn lock_breaks(old: &str) -> Vec<String> {
    let key = |l: &str| {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["msg", id, kind, ..] => Some((format!("msg {id}"), kind.to_string())),
            ["field", id, i, t, ..] => Some((format!("field {id} {i}"), t.to_string())),
            ["enum", e, v, ..] => Some((format!("enum {e} {v}"), String::new())),
            ["version", v] => Some(("version".into(), v.to_string())),
            _ => None,
        }
    };
    let now: std::collections::HashMap<String, String> = lock_text().lines().filter_map(key).collect();
    old.lines()
        .filter_map(key)
        .filter_map(|(k, v)| match now.get(&k) {
            Some(n) if *n == v => None,
            Some(n) => Some(format!("{k}: was {v}, now {n}")),
            None => Some(format!("{k} ({v}) is gone")),
        })
        .collect()
}
