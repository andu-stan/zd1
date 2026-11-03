//! The messages on the wire, table-driven from [`MESSAGES`]: the codec the host side runs (`host/core`, natively and
//! in the web page) and the reference the MCU's generated C is checked against (`protocol/gen` bakes vectors from
//! [`encode_vals`] into its test). The typed messages (`msg`) go through it too ([`Typed`]). The compatibility rules are the crate's: a decoder takes the fields it
//! knows, leaves the ones an older peer's message doesn't reach at zero / false / empty, ignores bytes after them (a
//! newer peer's); only a field cut in the middle or a length over its maximum is an error.

use crate::{MESSAGES, Message, Ty, enum_, message};
use alloc::{string::String, vec, vec::Vec};
use core::fmt;

/// A field's value: numbers (u8, u16, u32, enums), bools, and the bytes of a `str` (UTF-8 by convention) or `bytes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    N(u32),
    B(bool),
    S(Vec<u8>),
}

impl Val {
    /// What a field a message doesn't carry reads as.
    pub fn default_for(t: Ty) -> Val {
        match t {
            Ty::Bool => Val::B(false),
            Ty::Str(_) | Ty::Bytes(_) => Val::S(Vec::new()),
            _ => Val::N(0),
        }
    }
}

impl From<u32> for Val {
    fn from(n: u32) -> Val {
        Val::N(n)
    }
}
impl From<u16> for Val {
    fn from(n: u16) -> Val {
        Val::N(n as u32)
    }
}
impl From<u8> for Val {
    fn from(n: u8) -> Val {
        Val::N(n as u32)
    }
}
impl From<bool> for Val {
    fn from(b: bool) -> Val {
        Val::B(b)
    }
}
impl From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::S(s.as_bytes().to_vec())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Val {
        Val::S(s.into_bytes())
    }
}
impl From<&[u8]> for Val {
    fn from(b: &[u8]) -> Val {
        Val::S(b.to_vec())
    }
}
impl From<Vec<u8>> for Val {
    fn from(b: Vec<u8>) -> Val {
        Val::S(b)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The first byte names no message (or there is none).
    UnknownId(Option<u8>),
    /// A message by a name the spec doesn't have.
    UnknownMessage(String),
    /// A field the message doesn't have.
    UnknownField(&'static str, String),
    /// A value that isn't the field's type (a number for a string, ...), or an enum name the enum doesn't have.
    WrongType(&'static str, &'static str),
    /// A string or bytes over the field's maximum.
    TooLong(&'static str, &'static str, usize),
    /// A field cut in the middle.
    Short(&'static str, &'static str),
    /// A length over the field's maximum, or past the end.
    BadLength(&'static str, &'static str, usize),
    /// A bool that is neither 0 nor 1.
    NotBool(&'static str, &'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::UnknownId(None) => write!(f, "an empty message"),
            Error::UnknownId(Some(id)) => write!(f, "unknown message id {id:02x}"),
            Error::UnknownMessage(n) => write!(f, "no message {n}"),
            Error::UnknownField(m, n) => write!(f, "{m} has no field {n}"),
            Error::WrongType(m, n) => write!(f, "{m}.{n}: a value of another type"),
            Error::TooLong(m, n, l) => write!(f, "{m}.{n}: {l} bytes, over its maximum"),
            Error::Short(m, n) => write!(f, "{m}: short at {n}"),
            Error::BadLength(m, n, l) => write!(f, "{m}.{n}: bad length {l}"),
            Error::NotBool(m, n) => write!(f, "{m}.{n}: not a bool"),
        }
    }
}

/// A decoded message: its definition and a value for each of its fields, in order.
#[derive(Clone)]
pub struct Msg {
    pub def: &'static Message,
    pub vals: Vec<Val>,
}

impl PartialEq for Msg {
    fn eq(&self, o: &Msg) -> bool {
        self.def.id == o.def.id && self.vals == o.vals
    }
}
impl Eq for Msg {}
impl fmt::Debug for Msg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct(self.def.name);
        for (k, v) in self.fields() {
            d.field(k.name, v);
        }
        d.finish()
    }
}

impl Msg {
    pub fn name(&self) -> &'static str {
        self.def.name
    }

    /// The field `name`'s value. Panics on a name the message doesn't have: the names are the spec's, a typo is a bug.
    pub fn get(&self, name: &str) -> &Val {
        let i = self
            .def
            .fields
            .iter()
            .position(|f| f.name == name)
            .unwrap_or_else(|| panic!("{} has no field {name}", self.def.name));
        &self.vals[i]
    }

    /// A number field (u8, u16, u32, an enum's value).
    pub fn n(&self, name: &str) -> u32 {
        match self.get(name) {
            Val::N(n) => *n,
            v => panic!("{}.{name}: {v:?} is not a number", self.def.name),
        }
    }
    pub fn b(&self, name: &str) -> bool {
        match self.get(name) {
            Val::B(b) => *b,
            v => panic!("{}.{name}: {v:?} is not a bool", self.def.name),
        }
    }
    pub fn bytes(&self, name: &str) -> &[u8] {
        match self.get(name) {
            Val::S(s) => s,
            v => panic!("{}.{name}: {v:?} is not bytes", self.def.name),
        }
    }
    /// A `str` field as text (invalid UTF-8 replaced).
    pub fn s(&self, name: &str) -> String {
        String::from_utf8_lossy(self.bytes(name)).into_owned()
    }

    /// The fields with their values.
    pub fn fields(&self) -> impl Iterator<Item = (&'static crate::Field, &Val)> {
        self.def.fields.iter().zip(&self.vals)
    }
}

/// A message as a Rust type (`msg`, declared with the table): its values in the spec's order, and back.
pub trait Typed: Sized {
    const NAME: &'static str;
    fn vals(&self) -> Vec<Val>;
    /// From a decoded message of this name (the caller checked the name).
    fn from_msg(m: &Msg) -> Self;
}

/// A request, and what answers it.
pub trait Request: Typed {
    type Reply: Typed;
}

/// The encoding of a typed message.
pub fn encode_msg<M: Typed>(m: &M) -> Result<Vec<u8>, Error> {
    encode_vals(message(M::NAME).ok_or_else(|| Error::UnknownMessage(M::NAME.into()))?, &m.vals())
}

/// An enum's value by its name (any case).
pub fn enum_value(e: &str, name: &str) -> Option<u8> {
    enum_(e)?.values.iter().find(|v| v.0.eq_ignore_ascii_case(name)).map(|v| v.1)
}

/// An enum value's name (None for a value the enum doesn't have).
pub fn enum_name(e: &str, v: u32) -> Option<&'static str> {
    enum_(e)?.values.iter().find(|x| x.1 as u32 == v).map(|x| x.0)
}

/// The encoding of `m` with a value for every field, in order (the reference: the vectors come from here).
pub fn encode_vals(m: &'static Message, vals: &[Val]) -> Result<Vec<u8>, Error> {
    let mut v = vec![m.id];
    for (f, x) in m.fields.iter().zip(vals) {
        match (f.ty, x) {
            (Ty::U8 | Ty::Enum(_), Val::N(n)) => v.push(*n as u8),
            (Ty::Bool, Val::B(b)) => v.push(*b as u8),
            (Ty::U16, Val::N(n)) => v.extend((*n as u16).to_le_bytes()),
            (Ty::U32, Val::N(n)) => v.extend(n.to_le_bytes()),
            (Ty::Str(max), Val::S(s)) => {
                if s.len() > max as usize {
                    return Err(Error::TooLong(m.name, f.name, s.len()));
                }
                v.push(s.len() as u8);
                v.extend(s);
            }
            (Ty::Bytes(max), Val::S(s)) => {
                if s.len() > max as usize {
                    return Err(Error::TooLong(m.name, f.name, s.len()));
                }
                v.extend((s.len() as u16).to_le_bytes());
                v.extend(s);
            }
            _ => return Err(Error::WrongType(m.name, f.name)),
        }
    }
    Ok(v)
}

/// The message `name` with these fields by name (the others: zero, false, empty). An enum field also takes its
/// value's name as a string.
pub fn encode(name: &str, fields: &[(&str, Val)]) -> Result<Vec<u8>, Error> {
    let m = message(name).ok_or_else(|| Error::UnknownMessage(name.into()))?;
    for (k, _) in fields {
        if !m.fields.iter().any(|f| f.name == *k) {
            return Err(Error::UnknownField(m.name, (*k).into()));
        }
    }
    let vals: Vec<Val> = m
        .fields
        .iter()
        .map(|f| match (fields.iter().find(|(k, _)| *k == f.name).map(|(_, v)| v), f.ty) {
            (None, t) => Ok(Val::default_for(t)),
            (Some(Val::S(s)), Ty::Enum(e)) => core::str::from_utf8(s)
                .ok()
                .and_then(|s| enum_value(e, s))
                .map(|n| Val::N(n as u32))
                .ok_or(Error::WrongType(m.name, f.name)),
            (Some(v), _) => Ok(v.clone()),
        })
        .collect::<Result<_, _>>()?;
    encode_vals(m, &vals)
}

/// A message, under the compatibility rules (module doc).
pub fn decode(data: &[u8]) -> Result<Msg, Error> {
    let id = *data.first().ok_or(Error::UnknownId(None))?;
    let m = MESSAGES.iter().find(|m| m.id == id).ok_or(Error::UnknownId(Some(id)))?;
    let mut vals: Vec<Val> = m.fields.iter().map(|f| Val::default_for(f.ty)).collect();
    let mut k = 1;
    for (i, f) in m.fields.iter().enumerate() {
        if k == data.len() {
            break;
        }
        let n = match f.ty {
            Ty::U16 | Ty::Bytes(_) => 2,
            Ty::U32 => 4,
            _ => 1,
        };
        if k + n > data.len() {
            return Err(Error::Short(m.name, f.name));
        }
        let x = data[k..k + n].iter().rev().fold(0u32, |a, b| a << 8 | *b as u32);
        k += n;
        vals[i] = match f.ty {
            Ty::Str(_) | Ty::Bytes(_) => {
                let max = match f.ty {
                    Ty::Str(m) => m as usize,
                    Ty::Bytes(m) => m as usize,
                    _ => 0,
                };
                if x as usize > max || k + x as usize > data.len() {
                    return Err(Error::BadLength(m.name, f.name, x as usize));
                }
                let s = data[k..k + x as usize].to_vec();
                k += x as usize;
                Val::S(s)
            }
            Ty::Bool if x > 1 => return Err(Error::NotBool(m.name, f.name)),
            Ty::Bool => Val::B(x == 1),
            _ => Val::N(x),
        };
    }
    Ok(Msg { def: m, vals })
}

/// A list record (`Record`'s doc): entries one after another, each a u16 length (little-endian) and a message.
pub fn decode_list(data: &[u8]) -> Result<Vec<Msg>, Error> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < data.len() {
        if k + 2 > data.len() {
            return Err(Error::Short("list", "length"));
        }
        let n = u16::from_le_bytes([data[k], data[k + 1]]) as usize;
        k += 2;
        if k + n > data.len() {
            return Err(Error::BadLength("list", "entry", n));
        }
        out.push(decode(&data[k..k + n])?);
        k += n;
    }
    Ok(out)
}

/// A list record from its entries' encodings.
pub fn encode_list(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::new();
    for e in entries {
        v.extend((e.len() as u16).to_le_bytes());
        v.extend(e);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(m: &Message) -> Vec<Val> {
        m.fields
            .iter()
            .enumerate()
            .map(|(i, f)| match f.ty {
                Ty::Bool => Val::B(i % 2 == 0),
                Ty::Str(max) => Val::S((0..max).map(|j| b'a' + j % 26).collect()),
                Ty::Bytes(max) => Val::S((0..max).map(|j| j as u8).collect()),
                Ty::Enum(e) => Val::N(enum_(e).unwrap().values.last().unwrap().1 as u32),
                Ty::U8 => Val::N(0xA5),
                Ty::U16 => Val::N(0xBEEF),
                Ty::U32 => Val::N(0xDEAD_BEEF),
            })
            .collect()
    }

    #[test]
    fn round_trip_and_compatibility() {
        for m in MESSAGES {
            let vals = sample(m);
            let wire = encode_vals(m, &vals).unwrap();
            assert_eq!(decode(&wire).unwrap(), Msg { def: m, vals: vals.clone() }, "{}", m.name);
            // a newer peer's fields after ours: ignored
            let mut longer = wire.clone();
            longer.extend([7, 0xFF, 0]);
            assert_eq!(decode(&longer).unwrap().vals, vals, "{}: trailing bytes", m.name);
            // an older peer's message without the last field: its default
            if let Some(last) = m.fields.last() {
                let n = encode_vals(m, &vals[..vals.len() - 1]).unwrap();
                let mut want = vals.clone();
                *want.last_mut().unwrap() = Val::default_for(last.ty);
                assert_eq!(decode(&n).unwrap().vals, want, "{}: without its last field", m.name);
            }
        }
    }

    /// Every message's type carries its fields in the table's order and types: a decoded message through the type
    /// and back is the same.
    #[test]
    fn the_types_are_the_table() {
        for (name, round_trip) in crate::msg::ROUND_TRIPS {
            let m = message(name).unwrap();
            let msg = decode(&encode_vals(m, &sample(m)).unwrap()).unwrap();
            assert_eq!(round_trip(&msg), msg.vals, "{name}");
        }
        assert_eq!(crate::msg::ROUND_TRIPS.len(), MESSAGES.len());
        let w =
            encode_msg(&crate::msg::Select { source: crate::Source::Wifi, player: crate::Player::Airplay }).unwrap();
        assert_eq!(w, [0x12, 3, 1]);
    }

    #[test]
    fn by_name() {
        let w = encode("Select", &[("source", "wifi".into()), ("player", Val::N(1))]).unwrap();
        assert_eq!(w, [0x12, 3, 1]);
        assert_eq!(encode("Select", &[("nope", 1u8.into())]), Err(Error::UnknownField("Select", "nope".into())));
        assert_eq!(encode("Select", &[("source", "radio".into())]), Err(Error::WrongType("Select", "source")));
        assert!(matches!(encode("SetSetting", &[("key", Val::S([b'k'; 33].to_vec()))]), Err(Error::TooLong(..))));
        let r = decode(&encode("SettingReply", &[("value", "Kitchen".into())]).unwrap()).unwrap();
        assert_eq!((r.name(), r.s("value")), ("SettingReply", "Kitchen".into()));
    }

    #[test]
    fn lists() {
        let a = encode("ScanEntry", &[("ssid", "Home".into()), ("signal", Val::N(57)), ("security", "psk".into())])
            .unwrap();
        let b = encode("ScanEntry", &[("ssid", "Cafe".into()), ("signal", Val::N(80))]).unwrap();
        let l = encode_list(&[a.clone(), b.clone()]);
        assert_eq!(l.len(), a.len() + b.len() + 4);
        let d = decode_list(&l).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].s("ssid"), d[0].n("signal"), d[0].n("security")), ("Home".into(), 57, 1));
        assert_eq!((d[1].s("ssid"), d[1].n("security")), ("Cafe".into(), 0));
        assert!(decode_list(&[]).unwrap().is_empty());
        assert_eq!(decode_list(&[5, 0, 0x61]), Err(Error::BadLength("list", "entry", 5)));
        assert_eq!(decode_list(&[1]), Err(Error::Short("list", "length")));
    }

    #[test]
    fn refused() {
        assert_eq!(decode(&[]), Err(Error::UnknownId(None)));
        assert_eq!(decode(&[0xEE]), Err(Error::UnknownId(Some(0xEE))));
        assert_eq!(decode(&[0x7F, 1, 5, b'a']), Err(Error::BadLength("Error", "text", 5))); // past the end
        assert_eq!(decode(&[0x0F, 2]), Err(Error::NotBool("Lock", "open")));
        assert_eq!(decode(&[0x08, 1, 0]), Err(Error::Short("Module", "timeout_s"))); // a u16 cut in half
    }
}
