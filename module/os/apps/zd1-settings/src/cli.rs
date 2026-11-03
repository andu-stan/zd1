//! zd1-settings: the module OS's settings (docs/module/settings.md) from the shell and the init scripts.
//!   zd1-settings get KEY            the value in effect (secrets as "(set)")
//!   zd1-settings set KEY VALUE      store it (empty: back to the default), or do an action (wifi.add, wifi.forget,
//!                                   wifi.first, airplay.forget, reset); then apply it (init restarts what uses KEY)
//!   zd1-settings list               every setting, its value and whether that is the default
//!   zd1-settings keys               what each setting and action is
//!   zd1-settings networks           the known Wi-Fi networks, the preferred first (their keys never shown)
//!   zd1-settings pairings           the controllers paired with AirPlay
//!   zd1-settings wpa-conf           wpa_supplicant's configuration for the known networks (exit 1 without one)
//! The host sets them through the MCU: `zd1 settings` (zd1-linkd, the configuration channel).

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a[..] {
        ["get", k] => match crate::get(k) {
            Some(v) => println!("{}", crate::shown(k, &v)),
            None if crate::action(k).is_some() => println!(),
            None => die(&format!("no setting {k}")),
        },
        ["set", k, ref v @ ..] => {
            let v = v.join(" ");
            let r = if crate::action(k).is_some() { crate::act(k, &v) } else { crate::set(k, &v) };
            if let Err(e) = r {
                die(&e);
            }
            crate::apply(k);
        }
        ["list"] => {
            let all = crate::load();
            for f in crate::fields() {
                let key = f.key();
                let v = crate::get_from(&all, &key).unwrap_or_default();
                println!("{key}={}{}", crate::shown(&key, &v), if all.contains_key(&key) { "" } else { "  (default)" });
            }
        }
        ["keys"] => {
            for f in crate::fields() {
                println!("{:<16} {}", f.key(), f.doc);
            }
            for f in zd1_hostlink::actions() {
                println!("{:<16} (action) {}", f.key(), f.doc);
            }
        }
        ["networks"] => {
            for n in crate::networks() {
                println!("{}{}", n.ssid, if n.psk.is_some() { "  (WPA2)" } else { "  (open)" });
            }
        }
        ["pairings"] => {
            for (id, admin) in crate::pairings() {
                println!("{id}{}", if admin { "  (admin)" } else { "" });
            }
        }
        ["wpa-conf"] => {
            let nets = crate::networks();
            print!("{}", crate::wpa_conf(&nets));
            if nets.is_empty() {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: zd1-settings get KEY | set KEY VALUE | list | keys | networks | pairings | wpa-conf");
            std::process::exit(2);
        }
    }
}

fn die(e: &str) -> ! {
    zd1_sys::die("zd1-settings", e)
}
