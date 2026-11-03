//! The two declarations the spec is written in (lib.rs): `enums!` and `messages!`. Each declares the Rust types the
//! host side uses (an enum per enum, a struct per message, under `msg`) and the table everything else is made from
//! (`ENUMS`, `MESSAGES`: protocol/gen writes the MCU's C and the reference from it; the codec reads it). One
//! declaration, so the types and the table can't disagree.
//!
//! A field's type is one of u8, u16, u32, bool, String[MAX] (UTF-8, at most MAX bytes), Bytes[MAX], or an enum of the
//! spec. Docs are the items' `///` lines: rustdoc shows them, the table keeps them as one line.

/// The first of `$a` (when given) or `$b`.
macro_rules! or {
    (; $b:expr) => {
        $b
    };
    ($a:expr; $b:expr) => {
        $a
    };
}

macro_rules! gate {
    (gated) => {
        true
    };
    (free) => {
        false
    };
}
/// A message's `[module]` marker (a record of the module OS, relayed by the MCU, never decoded by it) and a request's
/// `emu` (only the emulator's build has it).
macro_rules! marker {
    (module) => {
        true
    };
    (emu) => {
        true
    };
}
/// A field's marker: `set` (a setting: the host may write it) or `mask` (which of the fields after it the request
/// changes: bit 0 the first).
macro_rules! is {
    (set set) => {
        true
    };
    (mask mask) => {
        true
    };
    ($a:ident $b:ident) => {
        false
    };
}

/// A message's struct: a unit struct without fields (`msg::Info`).
macro_rules! message_struct {
    ($(#[doc = $doc:literal])* $name:ident {}) => {
        $(#[doc = $doc])*
        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct $name;
    };
    ($(#[doc = $doc:literal])* $name:ident { $( $(#[doc = $fdoc:literal])* $f:ident : $t:ident, )+ }) => {
        $(#[doc = $doc])*
        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct $name { $( $(#[doc = $fdoc])* pub $f: $crate::types::$t, )+ }
    };
}

macro_rules! enums {
    ($( $(#[doc = $doc:literal])* $name:ident { $( $(#[doc = $vdoc:literal])* $v:ident = $val:expr, )* } )*) => {
        $(
            $(#[doc = $doc])*
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            pub enum $name {
                $( $(#[doc = $vdoc])* $v, )*
                /// A value this program doesn't know (a newer peer's).
                Unlisted(u8),
            }

            impl $name {
                pub const ALL: &'static [$name] = &[$($name::$v),*];

                pub const fn value(self) -> u8 {
                    match self { $($name::$v => $val,)* $name::Unlisted(v) => v }
                }

                /// The value's name in the spec (`Usb`; "?" for an unlisted one).
                pub const fn name(self) -> &'static str {
                    match self { $($name::$v => stringify!($v),)* $name::Unlisted(_) => "?" }
                }
            }

            impl From<u8> for $name {
                fn from(v: u8) -> $name {
                    $( if v == $val { return $name::$v; } )*
                    $name::Unlisted(v)
                }
            }

            impl Default for $name {
                fn default() -> $name { $name::from(0) }
            }

            impl core::str::FromStr for $name {
                type Err = $crate::NoSuchName;
                /// By its name, any case (`usb`, `USB`).
                fn from_str(s: &str) -> core::result::Result<$name, $crate::NoSuchName> {
                    $name::ALL.iter().copied().find(|v| v.name().eq_ignore_ascii_case(s)).ok_or($crate::NoSuchName)
                }
            }

            /// Its name in lower case (`usb`), or the number.
            impl core::fmt::Display for $name {
                fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                    match self {
                        $name::Unlisted(v) => write!(f, "{v}"),
                        v => v.name().chars().try_for_each(|c| core::fmt::Write::write_char(f, c.to_ascii_lowercase())),
                    }
                }
            }

            impl $crate::Wire for $name {
                const KIND: $crate::Kind = $crate::Kind::Enum(stringify!($name));
                fn to_val(&self) -> $crate::codec::Val { $crate::codec::Val::N(self.value() as u32) }
                fn from_val(v: &$crate::codec::Val) -> $name { $name::from(<u8 as $crate::Wire>::from_val(v)) }
            }

            impl $crate::SpecEnum for $name {
                const ALL: &'static [$name] = $name::ALL;
            }
        )*

        pub const ENUMS: &[Enum] = &[ $( Enum {
            name: stringify!($name),
            doc: concat!($($doc),*).trim_ascii_start(),
            values: &[ $( (stringify!($v), $val, concat!($($vdoc),*).trim_ascii_start()) ),* ],
        }, )* ];
    };
}

macro_rules! messages {
    ($( $(#[doc = $doc:literal])* $id:literal $name:ident
        $(-> $reply:ident [$gate:ident, $timeout:literal ms $(+ $wait:ident s or $wait_def:literal)? $(, $emu:ident)?])?
        $([$module:ident])?
        { $( $(#[doc = $fdoc:literal])* $f:ident : $t:ident $([$max:literal])? $($rw:ident $($fmt:ident)? $(= $def:literal)?)? ),* $(,)? } )*) => {
        /// The messages as Rust types: requests (each with its reply: `codec::Request`) and replies.
        pub mod msg {
            use $crate::codec::{Msg, Request, Typed, Val};
            use $crate::Wire;
            use alloc::{vec, vec::Vec};
            $(
                message_struct! { $(#[doc = $doc])* $name { $( $(#[doc = $fdoc])* $f: $t, )* } }

                impl Typed for $name {
                    const NAME: &'static str = stringify!($name);
                    fn vals(&self) -> Vec<Val> { vec![$( self.$f.to_val() ),*] }
                    #[allow(unused)]
                    fn from_msg(m: &Msg) -> $name {
                        let mut v = m.vals.iter();
                        $name { $( $f: <$crate::types::$t as Wire>::from_val(v.next().expect("a decoded message has every field")), )* }
                    }
                }

                $( impl Request for $name { type Reply = $reply; } )?
            )*

            /// Each message's typed round trip (for the codec's tests): decoded -> the type -> its values.
            #[cfg(test)]
            pub(crate) const ROUND_TRIPS: &[(&str, fn(&Msg) -> Vec<Val>)] = &[
                $( (stringify!($name), |m| <$name as Typed>::from_msg(m).vals()), )*
            ];
        }

        pub const MESSAGES: &[Message] = &[ $( Message {
            id: $id,
            name: stringify!($name),
            fields: &[ $( Field {
                name: stringify!($f),
                ty: Ty::of(<$crate::types::$t as Wire>::KIND, or!($($max)?; 0)),
                set: or!($(is!(set $rw))?; false),
                mask: or!($(is!(mask $rw))?; false),
                format: or!($($(stringify!($fmt))?)?; ""),
                default: or!($($($def)?)?; ""),
                doc: concat!($($fdoc),*).trim_ascii_start(),
            }, )* ],
            reply: or!($(Some(stringify!($reply)))?; None),
            gated: or!($(gate!($gate))?; false),
            module: or!($(marker!($module))?; false),
            timeout_ms: or!($($timeout)?; 0),
            wait: or!($($(Some((stringify!($wait), $wait_def)))?)?; None),
            emu: or!($($(marker!($emu))?)?; false),
            doc: concat!($($doc),*).trim_ascii_start(),
        }, )* ];
    };
}
