use crate::*;

#[test]
fn host_spec_is_consistent() {
    for (i, m) in MESSAGES.iter().enumerate() {
        assert!(MESSAGES[i + 1..].iter().all(|o| o.id != m.id && o.name != m.name), "{}: id and name unique", m.name);
        if m.module {
            // relayed in pieces (ModRecord), kept whole by the MCU and pushed in one SPI notification
            assert!(m.max_size() <= RECORD_MAX, "{}: {} bytes > RECORD_MAX", m.name, m.max_size());
            assert!(m.reply.is_none() && !m.gated, "{}: a record is neither a request nor gated", m.name);
        } else {
            assert!(m.max_size() <= MSG_MAX, "{}: {} bytes > MSG_MAX", m.name, m.max_size());
            assert!(m.max_size() + 5 <= POLL_MAX, "{}: a reply record fits a POLL", m.name);
        }
        if let Some(r) = m.reply {
            let r = message(r).unwrap_or_else(|| panic!("{}: reply {r} not defined", m.name));
            assert!(r.reply.is_none(), "{}: a reply is not a request", r.name);
        }
        for f in m.fields {
            if let Ty::Enum(e) = f.ty {
                assert!(enum_(e).is_some(), "{}.{}: enum {e}", m.name, f.name);
            }
            assert!(m.fields.iter().filter(|g| g.name == f.name).count() == 1, "{}.{} unique", m.name, f.name);
        }
    }
    for e in ENUMS {
        for (i, v) in e.values.iter().enumerate() {
            assert!(e.values[i + 1..].iter().all(|w| w.0 != v.0 && w.1 != v.1), "{}.{} unique", e.name, v.0);
        }
    }
    // the Peek limit in its doc is what Data holds
    let data = message("Data").unwrap();
    assert_eq!(data.fields[0].ty, Ty::Bytes(248));
}

#[test]
fn the_gate() {
    for m in MESSAGES {
        assert!(m.reply.is_some() || !m.gated, "{}: a reply is not gated", m.name);
    }
    for name in ["Hello", "Info", "Diag", "GetSetting", "ModRead", "Unlock"] {
        assert!(!message(name).unwrap().gated, "{name}: reads (and the way to open the gate) are free");
    }
    for name in [
        "Poke",
        "Peek",
        "Dfu",
        "Module",
        "ModWrite",
        "SetSetting",
        "Debug",
        "Amp",
        "Lock",
        "Key",
        "Select",
        "Volume",
        "XmFrame",
        "Pipe",
    ] {
        assert!(message(name).unwrap().gated, "{name} changes the speaker (or reads its RAM)");
    }
    let b = requests_bitmap();
    assert!(b[0] & 0x02 != 0 && b[0x41 / 8] & (1 << (0x41 % 8)) == 0, "requests only");
}

#[test]
fn timeouts_and_masks() {
    let m = message("Module").unwrap();
    assert_eq!(m.timeout(&[Val::N(2), Val::N(0)]), 130_000, "0: the MCU's 120 s");
    assert_eq!(m.timeout(&[Val::N(2), Val::N(60)]), 70_000);
    assert_eq!(message("Info").unwrap().timeout(&[]), 2000);
    let p = message("Prefs").unwrap();
    assert_eq!(["auto_switch", "amp_idle_s", "volume_cap"].map(|f| p.mask_bit(f)), [Some(1), Some(2), Some(4)]);
    assert_eq!(p.mask_bit("change"), None);
    assert!(message("PipeData").unwrap().emu && !message("Pipe").unwrap().emu);
}
